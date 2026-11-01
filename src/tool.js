/**
 * PRESET-plane Computer Use tools. Registers into the host `tools` registry
 * and provides nothing, so this row needs no isolate realm.
 * @module dsh-computer-use/tool
 */

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import crypto from 'node:crypto'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'
import Schema from '@deepseek-ai/schemastery'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { computerUsePrompt, documentationSummary, promptAssetStatus, BROWSER_REQUIRED_FOR } from './prompt.js'
import { getUiaHost, getUiaHostReady, prewarmUiaHost, closeUiaHost } from './uiahost-client.js'

export const name = 'tool-computer-use'
export const inject = ['tools', 'dshComputerUse', 'systemPrompt']

/**
 * Where the always-on computer-use contract sits in the system prompt. DSH 0.1.6 added
 * a centrally owned TOOL_COMPUTER_USE slot beside the other tool contracts; releases
 * without it return undefined, where the historical value 48 keeps the placement.
 * @param {object} systemPrompt the host system-prompt service
 * @returns {number}
 */
export function computerUseSectionOrder(systemPrompt) {
  try {
    const order = systemPrompt?.getSectionOrder?.('TOOL_COMPUTER_USE')
    return typeof order === 'number' ? order : 48
  } catch {
    return 48
  }
}

/** What to do when the helper reports `AppApprovalRequired` for a tool. */
export const APPROVAL_MODES = ['prompt', 'allow', 'deny']

/**
 * Request meta for one Computer Use tool call.
 *
 * The turn id MUST be stable for the whole DSH turn. The sidecar sends `end_turn`
 * whenever the turn scope changes (`ensureTurn`), and the helper treats that as "flush the
 * observation lease", so stamping the per-call id here made every call a new turn: an
 * observation was discarded before the very next input action, and the model saw
 * `coordinate input target is unavailable` / `call get_window_state before using this
 * window` on every click after a successful `get_window_state`. The turn id now comes from
 * the host service's `session/event` tracking; the fallback is a constant, never a call id.
 *
 * `callId` stays in the meta because the approval path forwards it to the answerer.
 *
 * @param {object} service host-plane `dshComputerUse` service
 * @param {object} exec tool execution context (`agent`, `callId`, `rootCallId`)
 * @returns {{ conversationId: string, turnId: string, callId: string }}
 */
export function turnMetaFor(service, exec) {
  const conversationId = String(exec?.agent?.id || exec?.rootCallId || 'dsh')
  const known = service?.currentTurnIdFor?.(conversationId)
  return {
    conversationId,
    turnId: known === undefined || known === null || known === '' ? 'turn' : String(known),
    callId: String(exec?.callId || ''),
  }
}

export const Config = Schema.object({
  surfaces: Schema.array(Schema.string()).default(['computer']),
  browserSkill: Schema.string().default('computer-use-browser'),
  /**
   * Per-tool approval policy (MCP-09). `prompt` asks the DSH approval service,
   * `allow` records the grant without asking, `deny` fails closed.
   *
   * Official elicits **only after** the helper refuses (APP-1), so this governs
   * the refusal path; the plugin never pre-approves an action the helper was
   * willing to take. The default is `prompt`: a method nobody configured must
   * never silently approve itself (APS-01).
   */
  approvalDefault: Schema.union(APPROVAL_MODES).default('prompt'),
  approvalTools: Schema.dict(Schema.union(APPROVAL_MODES)).default({}),
  /**
   * Desktop `enabled_tools` whitelist (MCP-07). The official `.mcp.json`
   * exposes only `js/js_reset/turn_ended`; DSH exposes the official 13 plus
   * explicitly listed harness extensions. Diagnostic tools
   * (`session_note/session_state/diagnostic_state`) and `end_turn` stay out of
   * the catalog unless named here.
   */
  // batch_actions and scroll_element are on by default because the always-present
  // `scroll` / `batch_actions` descriptions tell the model to use `scroll_element`
  // for indexed scrolling; a tool the guidance names must also be discoverable.
  // It is a harness-surface extension, so the official native `computer` surface
  // stays exactly 13. Diagnostic tools and end_turn stay opt-in.
  // `fill_form` joins the defaults because a multi-field form is the single most
  // common round-trip sink (each field costs an observe→act→observe cycle), and
  // the guidance now names it as the preferred path for 2+ field forms.
  enabledTools: Schema.array(Schema.string()).default(['batch_actions', 'scroll_element', 'fill_form']),
  /**
   * When true (default), the FIRST time an app's UIA provider wedges, that app's
   * accessibility text is served from the .NET ControlView fallback for the rest of
   * the session (a latch), so a flaky helper can never keep charging the 15s dump
   * timeout. Set false to keep the 60s-cooldown-then-retry behaviour.
   */
  fallbackLatchAfterWedge: Schema.boolean().default(true),
  // `preset` (V2 experience scope) was removed with the experience layer
  // (commits 0b11afb/bd2ef99); the option had no consumer left. Audit P2-10.
})

const OUTPUT_SCHEMA = {
  // `value` carries arbitrary tool output (object, array, string, number...), so the
  // schema is intentionally open. The host validator rejects an untyped `properties.value`
  // (both `{}` and `{type:'json'}` fail), so we declare no properties at all and accept
  // any object shape via additionalProperties.
  type: 'object',
  additionalProperties: true,
}

const BROWSER_SKILL = 'computer-use-browser'

/**
 * Official `docs/documents.json` `requiredFor` entries that ship with DSH.
 * Reading the listed reference clears the gate (PSG-5); the official runtime
 * enforces the same table with `assertRequiredDocumentationRead`.
 *
 * PSG-4: the value is a **path-qualified** reference, not a basename. The desktop
 * and browser skills both ship a `references/confirmations.md` with different
 * policy text, so matching on the basename let a read of the *Windows* policy
 * satisfy the *browser* CDP gate without the official browser confirmations ever
 * being read.
 */
const BROWSER_CONFIRMATIONS_DOC = 'computer-use-browser/references/confirmations.md'

export const REQUIRED_DOCS = {
  // Invert the official documents.json shape (document -> tools) into the lookup
  // the dispatcher needs (tool -> documents).
  ...Object.fromEntries(
    BROWSER_REQUIRED_FOR[BROWSER_CONFIRMATIONS_DOC].map(tool => [tool, [BROWSER_CONFIRMATIONS_DOC]]),
  ),
  // The official entry for this one points at the unshipped
  // capabilities/tab/browserAuth document; until that ships, the browser
  // confirmations policy is the shipped gate.
  tab_browser_auth_handoff: [BROWSER_CONFIRMATIONS_DOC],
}

/** Normalise a `read` file path so the gate is separator- and case-insensitive. */
export function normalizeDocPath(file) {
  return String(file).replace(/\\/g, '/').toLowerCase()
}

/** Whether a recorded read satisfies a path-qualified `requiredFor` entry. */
export function documentWasRead(readPaths, required) {
  const needle = normalizeDocPath(required)
  for (const read of readPaths) {
    const path = normalizeDocPath(read)
    if (path === needle || path.endsWith('/' + needle)) return true
  }
  return false
}

/**
 * Official documents referenced by `requiredFor` that DSH does not ship yet
 * (`capabilities/tab/cdp`, `webmcp`, `capabilities/tab/browserAuth`). Kept
 * visible in health so the gap is auditable instead of silent.
 */
export const UNSHIPPED_REQUIRED_DOCS = ['capabilities/tab/cdp', 'webmcp', 'capabilities/tab/browserAuth']

/**
 * Browser entry selection, layered onto every browser tool description
 * (MCP-03). Official `resources/browser-description.md` is concatenated into
 * the `js` tool description; DSH has one description per tool, so the same
 * first-match strategy is prepended to the browser catalog.
 */
const BROWSER_ENTRY_GUIDANCE = [
  'Browser entry selection - use the first matching option:',
  '1. Tab @-mention: find the tab whose id/title/url match the mention, then attach it.',
  '2. Known tab id and browser: attach that tab.',
  '3. Known URL in the in-app browser: create a visible in-app tab.',
  '4. Known URL and a named browser: create a tab in that browser; do not probe first.',
  '5. Known URL and no named browser: select the browser for that URL.',
].join('\n')

export function apply(ctx, config) {
  const surfaces = Array.isArray(config.surfaces) ? config.surfaces : ['computer']
  const desktop = surfaces.length === 0 || surfaces.includes('computer') || surfaces.includes('all')
  const wantsBrowser = surfaces.includes('browser') || surfaces.includes('all')
  const gateBrowser = !wantsBrowser
  const state = {
    browserUnlocked: false,
    exposedNames: [],
    surfaces,
    /** Basenames of reference documents the model has actually read (PSG-5). */
    readDocuments: new Set(),
    /** Audit trail for operator-configured `allow` grants (APS-01). */
    recordedApprovals: [],
  }
  const hideOverlay = () => {
    ctx.dshComputerUse.releaseOverlay().catch(() => {})
  }
  // No `session/event` listener here on purpose. This row is mounted in the user-agent
  // preset, i.e. in a descendant scope, and scope-filtered dispatch only reaches a
  // listener on an equal or enclosing scope, so a listener here can never see
  // `turn/end` (the old hook was dead code and left the overlay on screen). The
  // host-plane service in `index.js` owns the turn-end release.
  ctx.effect(() => () => {
    hideOverlay()
    ctx.dshComputerUse.shutdownSidecar().catch(() => {})
  }, 'computer-use overlay release')

  ctx.systemPrompt.section({
    name: 'tool:computer-use',
    // 0.1.6+ reserves the TOOL_COMPUTER_USE slot (3000) for the computer-use contract;
    // older releases have no such slot, where 48 keeps the previous placement.
    order: computerUseSectionOrder(ctx.systemPrompt),
    // Always-on text is the session contract plus the on-demand pointer; the
    // reference documents resolve through the skill's references/ directory.
    text: () => computerUsePrompt(),
  })

  // One listener serves both hooks: the skill result unlocks the browser
  // catalog, and any `read` of a bundled reference clears the requiredFor gate.
  ctx.on('tools/result', (exec, result) => {
    if (result?.isError) return
    if (exec.name === 'read') {
      const file = String(exec.arguments?.file_path || exec.arguments?.path || '')
      if (file) state.readDocuments.add(normalizeDocPath(file))
      return
    }
    if (!gateBrowser || exec.name !== 'skill') return
    if (String(exec.arguments?.name || '') === BROWSER_SKILL) void unlockBrowserTools(ctx, state, config)
  })

  const ready = desktop ? registerDesktop(ctx, state, config) : Promise.resolve()
  if (wantsBrowser) return Promise.resolve(ready).then(() => unlockBrowserTools(ctx, state, config))
  return ready
}

/**
 * Register one tool and remember its real name (P2-6).
 *
 * `computer_use_health.tools.exposed` used to be a hardcoded list sitting next to the
 * registrations, so the 2026-10 `computer_use_*` rename left it printing names that no
 * longer existed while the registry carried the new ones. The health surface now
 * reports what was actually registered.
 */
function registerTool(ctx, state, tool) {
  ctx.tools.register(tool)
  const name = tool && typeof tool.name === 'string' ? tool.name : ''
  if (!name) return
  state.registeredNames = Array.isArray(state.registeredNames) ? state.registeredNames : []
  if (!state.registeredNames.includes(name)) state.registeredNames.push(name)
}

async function registerDesktop(ctx, state, config) {
  let desktops
  try {
    const listed = await ctx.dshComputerUse.tools('computer')
    desktops = Array.isArray(listed?.tools) ? listed.tools.filter(spec => spec?.name) : []
  } catch (error) {
    registerTool(ctx, state, healthTool(ctx, state, config, String(error)))
    return
  }
  // Harness extensions are requested as their own surface (TC-01). A helper
  // that does not know `harness` yet falls back to the full table, so the
  // name-dedup below still yields exactly the non-window2 extras.
  let harness = []
  for (const surface of ['harness', 'gated']) {
    if (harness.length > 0) break
    try {
      const listed = await ctx.dshComputerUse.tools(surface)
      const known = new Set(desktops.map(spec => spec.name))
      harness = (Array.isArray(listed?.tools) ? listed.tools : [])
        .filter(spec => spec?.name && !known.has(spec.name))
    } catch {
      harness = []
    }
  }
  const enabled = new Set(config.enabledTools || [])
  const extras = harness.filter(spec => enabled.has(spec.name))
  // "First wedge → permanently use fallback for this app this session" switch.
  state.fallbackLatch = config.fallbackLatchAfterWedge !== false
  registerTool(ctx, state, healthTool(ctx, state, config))
  // Standalone, helper-independent tools. They talk to the sidecar only through the
  // same `call` bridge the window2 tools use, so they need no helper surface entry
  // (and cannot disturb the `WINDOW2_TOOLS` contract).
  // NOTE: there is NO harness-provided desktop `wait` — an earlier assumption of that
  // was wrong; this plugin owns it. Register it once, here.
  registerTool(ctx, state, waitTool(ctx, state))
  // Read-only CDP probe (detect/enable CDP for Chromium/WebView2 hosts).
  registerTool(ctx, state, cdpProbeTool(ctx, state))
  // Long-document + polling helpers (one-call scroll-to-end, wait-until, doc URLs).
  registerTool(ctx, state, scrollToTool(ctx, state))
  registerTool(ctx, state, reachLastPageTool(ctx, state))
  registerTool(ctx, state, waitUntilTool(ctx, state))
  registerTool(ctx, state, documentsTool(ctx, state))
  registerTool(ctx, state, uiaActTool(ctx, state))
  // P0-3 / P0-4: read and set tick states through the persistent UIA host.
  registerTool(ctx, state, readOptionsTool(ctx, state))
  registerTool(ctx, state, setOptionsTool(ctx, state))
  // Warm the host in the background so its ~3.5 s PowerShell start-up is paid at plugin
  // load, not on the first read_options call. Fire-and-forget: a failure here is harmless
  // (the first real call retries) and it must never delay or fail plugin registration.
  if (config.prewarmUiaHost !== false) {
    setTimeout(() => { prewarmUiaHost().catch(() => { }) }, 0)
  }
  const exposed = desktops.concat(extras)
  for (const spec of exposed) {
    try {
      registerTool(ctx, state, makeSidecarTool(ctx, spec, config, state, {}))
    } catch (error) {
      console.error(`[dsh-computer-use] skipped tool ${spec.name}: ${error}`)
    }
  }
  // P2-6: publish exactly what the registry received, in registration order.
  state.exposedNames = [...(state.registeredNames || [])]
  publishToolNames(ctx, state)
}

/**
 * Tell the safety contract which tool schemas belong to this plugin.
 *
 * The contract hides them from a session whose capability is off (SEC-02), so it
 * needs the exact set of names — the list grows when the browser catalog is
 * unlocked, which is why it is published again from there.
 *
 * @param {object} ctx
 * @param {{ exposedNames: string[] }} state
 */
function publishToolNames(ctx, state) {
  const safety = ctx.dshComputerUse?.safety
  if (safety && typeof safety.setToolNames === 'function') {
    safety.setToolNames(state.exposedNames)
  }
}

async function unlockBrowserTools(ctx, state, config) {
  if (state.browserUnlocked) return
  state.browserUnlocked = true
  let listed
  try {
    listed = await ctx.dshComputerUse.tools('browser')
  } catch (error) {
    console.error(`[dsh-computer-use] browser tools failed to load: ${error}`)
    state.browserUnlocked = false
    return
  }
  for (const spec of listed?.tools || []) {
    if (!spec?.name) continue
    try {
      registerTool(ctx, state, makeSidecarTool(ctx, spec, config, state, { descriptionPrefix: BROWSER_ENTRY_GUIDANCE }))
      state.exposedNames = [...(state.registeredNames || [])]
    } catch (error) {
      console.error(`[dsh-computer-use] skipped tool ${spec.name}: ${error}`)
    }
  }
  publishToolNames(ctx, state)
  try {
    await ctx.dshComputerUse.call('browser_setup', { environment: 'codex-app' })
  } catch (error) {
    console.error(`[dsh-computer-use] browser_setup failed: ${error}`)
  }
}

function healthTool(ctx, state, config, startError) {
  return defineTool({
    name: 'computer_use_health',
    description:
      'Report whether Computer Use is running, which desktop backend is active, the observation freshness model ' +
      '(window identity, bounds, and the human-input monitor), the app allow list, the injected helper environment, ' +
      'and whether browser tools are unlocked. Codex is not required.',
    parameters: {},
    output: {
      schema: { type: 'object', additionalProperties: true },
      render(_args, value) {
        return [{ type: 'text', text: JSON.stringify(value, null, 2) }]
      },
    },
    async execute(_args, exec) {
      const catalogue = {
        browserUnlocked: Boolean(state.browserUnlocked),
        browserSkill: BROWSER_SKILL,
        tools: { exposed: state.exposedNames, enabled: config.enabledTools },
        // P0-1: how many observations needed the repaint nudge / PrintWindow / stale
        // frame, so "the capture keeps failing" is answerable from health instead of
        // from the model's memory of a timeout.
        captureRecovery: {
          attempts: Object.fromEntries(state.captureStats instanceof Map ? state.captureStats : new Map()),
          nudgedWindows: state.nudgeNext instanceof Set ? [...state.nudgeNext] : [],
          cachedFrames: state.frameCache instanceof Map ? [...state.frameCache.keys()] : [],
        },
        fallback: {
          latchAfterWedge: config.fallbackLatchAfterWedge !== false,
          latchedApps: state.wedgeLatched instanceof Set ? [...state.wedgeLatched] : [],
        },
        // The safety contract, seen from the calling session: may it drive the
        // desktop, does it hold the write lease, and are its tool schemas
        // currently hidden. This is the audit surface for "why was that
        // refused" — the answer belongs next to the health facts.
        safety: ctx.dshComputerUse?.safetyState?.(exec?.agent?.session?.id ?? exec?.agent?.id),
        approval: {
          default: config.approvalDefault,
          tools: config.approvalTools,
          recorded: state.recordedApprovals.slice(0, 32),
          // What this session will actually do with a helper refusal: full access and a
          // 'never' policy both grant it without an ask (see approvalFacts).
          session: approvalFacts(exec, ctx.get('approval')),
        },
        documentation: documentationSummary(),
        promptAssets: promptAssetStatus(),
        documentationGate: {
          requiredFor: REQUIRED_DOCS,
          unshipped: UNSHIPPED_REQUIRED_DOCS,
          read: [...state.readDocuments],
        },
      }
      if (startError) {
        return { ok: false, codexRequired: false, error: startError, ...catalogue }
      }
      const health = await ctx.dshComputerUse.health()
      // The overlay's own diagnostics (which pill renderer is live, how many frames it
      // pushed, whether the pill window exists at all) live on the helper's
      // diagnostic_state method. Surfacing them here answers "the pill did not appear"
      // from the documented health call instead of a bespoke probe: the status pill is a
      // DirectComposition visual tree, so when it is missing nothing in the normal API says
      // why. The helper may be gone or wedged, so this never fails the health call.
      let overlay
      try {
        const diagnostics = await ctx.dshComputerUse.call('diagnostic_state', {})
        const payload = diagnostics && diagnostics.value !== undefined ? diagnostics.value : diagnostics
        overlay = payload && typeof payload === 'object' ? payload.overlayState : undefined
      } catch {
        overlay = undefined
      }
      return { ...health, ...catalogue, ...(overlay === undefined ? {} : { overlay }) }
    },
    isConcurrencySafe() {
      return true
    },
    presentCall() {
      return { card: 'generic', title: 'computer_use_health' }
    },
  })
}

// ---------------------------------------------------------------------------
// Standalone, helper-independent tools: `wait` and `computer_use_cdp_probe`.
// ---------------------------------------------------------------------------

const WAIT_MAX_MS = 60_000

/** Sleeping that honours the tool-call abort signal. */
function sleepRespectingSignal(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) { reject(new Error('wait aborted')); return }
    const timer = setTimeout(() => { cleanup(); resolve() }, ms)
    const onAbort = () => { cleanup(); reject(new Error('wait aborted')) }
    const cleanup = () => { clearTimeout(timer); signal?.removeEventListener?.('abort', onAbort) }
    signal?.addEventListener?.('abort', onAbort)
  })
}

/**
 * Desktop `wait`: the missing counterpart of the browser's `tab_pw_wait_for_timeout`.
 * Auto-playing media, animations, debounced search boxes and polling loops all need
 * a pause; without this the model either wastes a helper round-trip or fakes it with
 * a shell sleep. `observe:true` folds a fresh get_window_state into the same call so
 * the very next action is grounded in current state.
 */
function waitTool(ctx, state) {
  return defineTool({
    name: 'computer_use_wait',
    description:
      'Wait a fixed duration without a helper round-trip. Use it for auto-playing media, animations, debounced UI, '
      + 'or between polls. Set observe:true with a window to also take a fresh get_window_state (screenshot, and '
      + 'optionally the accessibility tree) after the wait so the next action is grounded in current state. '
      + 'Desktop counterpart of the browser tab wait.',
    parameters: {
      ms: { type: 'integer', description: 'Milliseconds to wait (default 1000, clamped to 0..60000).' },
      window: {
        type: 'object',
        additionalProperties: false,
        description: 'Optional target window to observe after the wait (required when observe:true).',
        properties: {
          app: { type: 'string', required: true },
          id: { type: 'integer', required: true },
          title: { type: 'string' },
        },
      },
      observe: { type: 'boolean', description: 'When true and a window is given, take a fresh get_window_state after waiting (default false).' },
      include_text: {
        description: 'With observe:true, also return the accessibility tree. true = full indexed tree; '
          + '"summary" = document text + interactive elements only (≈10x fewer tokens, use this when you '
          + 'only need to read the page or pick an element); default false.',
        oneOf: [{ type: 'boolean' }, { type: 'string', enum: ['summary'] }],
      },
    },
    output: { schema: OUTPUT_SCHEMA, render(_args, value) { return renderValue(value) } },
    async execute(args, exec) {
      const raw = Number(args?.ms ?? 1000)
      const ms = Number.isFinite(raw) ? Math.min(Math.max(Math.trunc(raw), 0), WAIT_MAX_MS) : 1000
      await sleepRespectingSignal(ms, exec?.signal)
      const out = { waitedMs: ms }
      if (args?.observe && args?.window) {
        const turnMeta = turnMetaFor(ctx.dshComputerUse, exec)
        const wantText = args.include_text === true || args.include_text === 'summary'
        const res = await observeWithLadder(ctx, state, exec, turnMeta, args.window, { include_text: wantText })
        if (res && res.ok === false && res.error) throw new Error(String(res.error))
        const value = res && res.value !== undefined ? res.value : res
        if (args.include_text === 'summary' && value && typeof value === 'object') {
          // P1-6: a page you only need to READ does not deserve the whole container tree.
          // Keep the document text plus one line per interactive/interesting node.
          if (value.accessibility && typeof value.accessibility === 'object') {
            const full = String(value.accessibility.tree || '')
            value.accessibility.tree = summarizeTree(full)
            value.accessibility.summary = true
            value.accessibility.fullLength = full.length
          }
          if (Array.isArray(value.screenshots)) value.screenshots = value.screenshots.slice(0, 1)
        }
        if (value?.accessibility?.tree) {
          state.lastElements = extractInteractiveElements(value.accessibility.tree)
          state.lastWindow = { app: args.window.app, id: args.window.id, title: args.window.title }
        }
        const refs = await admitImages(ctx, res?.images || [])
        return { value: { ...out, observation: value }, images: refs }
      }
      return { value: out, images: [] }
    },
    isConcurrencySafe() { return true },
    presentCall(args) { return { card: 'generic', title: 'computer_use_wait', rawInput: args && typeof args === 'object' ? args : {} } },
  })
}

/** Win32 process rows for a name/path, via a single read-only CIM query. */
function listProcessRows(filter) {
  const script = `$ErrorActionPreference='SilentlyContinue'; Get-CimInstance Win32_Process${filter} | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress`
  try {
    const raw = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
      encoding: 'utf8', timeout: 8000, windowsHide: true, maxBuffer: 8 * 1024 * 1024,
    }).trim()
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : [parsed]
  } catch {
    return []
  }
}

/** Read a Chromium `DevToolsActivePort` file: line 1 is the port, line 2 the path. */
function readDevToolsActivePort(file) {
  try {
    const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/)
    const port = Number(String(lines[0] || '').trim())
    if (Number.isInteger(port) && port > 0 && port < 65536) {
      return { port, wsPath: String(lines[1] || '').trim() }
    }
  } catch { /* not present */ }
  return null
}

async function fetchCdpVersion(port) {
  try {
    const res = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(900) })
    if (!res.ok) return null
    const data = await res.json()
    return { port, browser: data?.Browser ?? null, webSocketDebuggerUrl: data?.webSocketDebuggerUrl ?? null }
  } catch {
    return null
  }
}

/**
 * Is this window's process a Chromium/WebView2 host whose CDP endpoint is reachable?
 * WeChat's mini-program runtime (WeChatAppEx / RadiumWMPF) is Chromium-derived but does
 * NOT honour --remote-debugging-port and exposes no DevToolsActivePort, so it can only
 * ever be pixel-driven — this probe says so explicitly instead of letting the model
 * keep hunting for a DOM route that cannot exist.
 */
async function probeCdp(args) {
  let exe = ''
  if (typeof args.process === 'string' && args.process.trim()) exe = args.process.trim()
  else if (args.window && typeof args.window.app === 'string') {
    const m = /^process:(.+)$/i.exec(args.window.app)
    exe = m ? m[1] : args.window.app
  }
  let rows = []
  if (Number.isInteger(args.pid) && args.pid > 0) {
    rows = listProcessRows(` -Filter "ProcessId=${args.pid}"`)
  } else if (exe) {
    const base = path.basename(exe).replace(/'/g, "''")
    rows = listProcessRows(` -Filter "Name='${base}'"`)
    const wantPath = exe.toLowerCase()
    if (rows.some(r => r.ExecutablePath)) {
      const matched = rows.filter(r => String(r.ExecutablePath || '').toLowerCase() === wantPath)
      if (matched.length) rows = matched
    }
  }
  const ports = new Set()
  const pipePids = []
  const devToolsActivePorts = []
  for (const row of rows) {
    const cmd = String(row.CommandLine || '')
    const portMatch = /--remote-debugging-port=(\d+)/i.exec(cmd)
    if (portMatch) ports.add(Number(portMatch[1]))
    if (/--remote-debugging-pipe/i.test(cmd)) pipePids.push(Number(row.ProcessId))
    const ud = /--user-data-dir="?([^"\s]+)"?/i.exec(cmd) || /--webview-user-data-folder="?([^"\s]+)"?/i.exec(cmd)
    if (ud) {
      const found = readDevToolsActivePort(path.join(ud[1], 'DevToolsActivePort'))
      if (found) { devToolsActivePorts.push({ file: path.join(ud[1], 'DevToolsActivePort'), ...found }); ports.add(found.port) }
    }
  }
  const endpoints = []
  for (const port of ports) {
    const version = await fetchCdpVersion(port)
    if (version) endpoints.push(version)
  }
  const blob = rows.map(r => `${r.Name || ''} ${r.ExecutablePath || ''} ${r.CommandLine || ''}`).join(' ')
  const chromiumLike = /chrome|chromium|msedge|electron|webview2|WeChatAppEx|RadiumWMPF|wmpf/i.test(blob)
  const canAttach = endpoints.length > 0
  let guidance
  if (canAttach) {
    const port = endpoints[0].port
    guidance = `CDP is reachable on 127.0.0.1:${port}. Attach chrome-devtools MCP with --browserUrl http://127.0.0.1:${port} and drive the DOM instead of pixels.`
  } else if (chromiumLike) {
    guidance = exe && /WeChatAppEx|RadiumWMPF|wmpf/i.test(`${exe} ${blob}`)
      ? 'Chromium-derived host (WeChat WMPF/Radium) with NO CDP endpoint. It ignores --remote-debugging-port and writes no DevToolsActivePort, so DOM automation is not available — drive this window by screenshot + coordinates.'
      : 'Chromium/WebView2 host but no CDP endpoint is exposed right now. To enable it, relaunch that app with --remote-debugging-port=<free> (or, for a WebView2 host, set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<free> before launch), then probe again.'
  } else {
    guidance = 'The target process does not look like a Chromium/WebView2 host; CDP does not apply. Use the window2 pixel/UIA path.'
  }
  return {
    probed: { exe: exe || null, pids: rows.map(r => Number(r.ProcessId)), matchedProcesses: rows.length },
    chromiumLike,
    remoteDebuggingPorts: [...ports],
    remoteDebuggingPipePids: pipePids,
    devToolsActivePorts,
    endpoints,
    canAttach,
    guidance,
  }
}

function cdpProbeTool() {
  return defineTool({
    name: 'computer_use_cdp_probe',
    description:
      'Detect whether a window belongs to a Chromium/WebView2 host and whether its CDP endpoint is reachable, '
      + 'so DOM-level automation can replace pixel clicks when possible. Reports the debugging port/ws endpoint, '
      + 'DevToolsActivePort files, or the exact enablement step. WeChat WMPF/Radium renderers expose no CDP and stay '
      + 'pixel-driven. Read-only.',
    parameters: {
      window: {
        type: 'object',
        additionalProperties: false,
        description: 'Window to probe (its app identifier names the process).',
        properties: {
          app: { type: 'string', required: true },
          id: { type: 'integer', required: true },
          title: { type: 'string' },
        },
      },
      process: { type: 'string', description: 'Process name or full exe path to probe instead of a window.' },
      pid: { type: 'integer', description: 'Explicit process id to probe.' },
    },
    output: { schema: OUTPUT_SCHEMA, render(_args, value) { return renderValue(value) } },
    async execute(args) {
      return { value: await probeCdp(args && typeof args === 'object' ? args : {}) }
    },
    isConcurrencySafe() { return true },
    presentCall() { return { card: 'generic', title: 'computer_use_cdp_probe' } },
  })
}

// ---------------------------------------------------------------------------
// Long-document / polling helpers: scroll_to, reach_last_page, wait_until,
// document-URL export, and region cropping.
// ---------------------------------------------------------------------------

// Author-facing value schema for a window handle. `required` is a per-property
// annotation in the harness DSL; an object node must NOT carry a `required` array
// (schema.ts assertAuthorKeys rejects it).
const WIN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    app: { type: 'string', required: true },
    id: { type: 'integer', required: true },
    title: { type: 'string' },
  },
}

/** A `window` parameter (required) built from {@link WIN_SCHEMA}. */
const WIN_PARAM = { ...WIN_SCHEMA, required: true }

/** Write a base64 image to a temp file, crop it with System.Drawing, return base64 PNG. */
const CROP_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'scripts', 'crop-image.ps1')

export function normalizeRegion(region) {
  if (!region || typeof region !== 'object') return null
  const x = Math.max(0, Math.trunc(Number(region.x) || 0))
  const y = Math.max(0, Math.trunc(Number(region.y) || 0))
  const width = Math.trunc(Number(region.width) || 0)
  const height = Math.trunc(Number(region.height) || 0)
  if (width <= 0 || height <= 0) return null
  return { x, y, width, height }
}

/**
 * Crop a captured frame to a window-relative rectangle.
 *
 * The captured frame's pixel (0,0) is NOT necessarily window (0,0): the helper
 * reports `originX`/`originY` (typically -8/-8 for the invisible window border).
 * Cropping must therefore subtract that origin and clamp inside the frame,
 * otherwise a rectangle near the bottom edge collapses to a 1-pixel strip.
 */
export function cropImageBase64(base64, mimeType, region, origin, frame) {
  const rg = normalizeRegion(region)
  if (!rg || !base64) return null
  const ox = origin && Number.isFinite(origin.x) ? origin.x : 0
  const oy = origin && Number.isFinite(origin.y) ? origin.y : 0
  let x = Math.max(0, Math.round(rg.x - ox))
  let y = Math.max(0, Math.round(rg.y - oy))
  let w = Math.round(rg.width)
  let h = Math.round(rg.height)
  if (frame && Number.isFinite(frame.width)) w = Math.min(w, Math.max(1, frame.width - x))
  if (frame && Number.isFinite(frame.height)) h = Math.min(h, Math.max(1, frame.height - y))
  const inExt = /png/i.test(String(mimeType || '')) ? 'png' : 'jpg'
  let dir = null
  try {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cu-crop-'))
    const inPath = path.join(dir, `in.${inExt}`)
    const outPath = path.join(dir, 'out.png')
    fs.writeFileSync(inPath, Buffer.from(base64, 'base64'))
    execFileSync('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', CROP_SCRIPT,
      '-InPath', inPath, '-OutPath', outPath,
      '-X', String(x), '-Y', String(y), '-W', String(w), '-H', String(h),
    ], { timeout: 15_000, windowsHide: true })
    return fs.readFileSync(outPath).toString('base64')
  } catch {
    return null
  } finally {
    try { if (dir) fs.rmSync(dir, { recursive: true, force: true }) } catch { /* ignore */ }
  }
}

// ---------------------------------------------------------------------------
// Capture degradation (P0-1): Windows Graphics Capture needs a repaint, and a
// static window (exam form, paused player, document) never produces one. The
// helper therefore answers `window capture timed out` and the observation was
// useless even though `health.capture.coverage.knownPaths` lists GDI paths.
// ---------------------------------------------------------------------------

/** Nudge-only repaint / PrintWindow pixel grab, plus the screen geometry to explain a failure. */
const GDI_CAPTURE_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'scripts', 'gdi-capture.ps1')

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
 * Parse the single JSON line a helper script prints.
 *
 * `gdi-capture.ps1` and `uia-act.ps1` both print their verdict as one JSON line and
 * THEN exit non-zero on failure, so `execFileSync` throws with the payload sitting on
 * `error.stdout`. Reading only `error.message` threw that away and surfaced
 * `Command failed: powershell.exe …` instead of "no element matching …", which is the
 * one sentence the model needs to fix its call.
 */
export function parseJsonLine(text) {
  const line = String(text || '').split(/\r?\n/).find((l) => l.trim().startsWith('{'))
  if (!line) return null
  try { return JSON.parse(line) } catch { return null }
}

/** Force a repaint so the helper's own WGC capture session can produce a frame. */
export function nudgeWindow(hwnd) {
  return runCaptureScript(hwnd, { nudgeOnly: true })
}

/** Interpolated SendInput swipe (no helper capture session needed). */
const UIA_SWIPE_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'scripts', 'uia-swipe.ps1')

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
 * Grab a window with PrintWindow(PW_RENDERFULLCONTENT) as base64 PNG.
 *
 * Visual-only fallback: the helper never saw this frame, so it cannot be the
 * lease behind a coordinate click. It still beats "no screenshot at all" —
 * the model can read the page and act by element name or keyboard.
 */
export function gdiWindowFrame(hwnd) {
  let dir = null
  try {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cu-gdi-'))
    const outPath = path.join(dir, 'frame.png')
    const res = runCaptureScript(hwnd, { outPath })
    if (!res || res.ok !== true) return { ok: false, error: (res && res.error) || 'capture failed', geometry: res }
    if (!fs.existsSync(outPath)) return { ok: false, error: 'no png written' }
    const buf = fs.readFileSync(outPath)
    if (!buf.length) return { ok: false, error: 'empty png' }
    return {
      ok: true,
      data: buf.toString('base64'),
      width: Number(res.width) || 0,
      height: Number(res.height) || 0,
      geometry: res,
    }
  } catch (error) {
    return { ok: false, error: String(error && error.message ? error.message : error) }
  } finally {
    try { if (dir) fs.rmSync(dir, { recursive: true, force: true }) } catch { /* ignore */ }
  }
}

/** Shape a raw frame into the helper's observation shape (image + screenshot entry). */
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

function bumpCaptureStat(state, key) {
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
export function summarizeTree(tree) {
  const text = String(tree || '')
  if (!text.trim()) return text
  const lines = text.split(/\r?\n/)
  const keep = []
  // Text is the page's actual content (a reader needs it), so it is kept — the things
  // worth dropping are pure containers. Getting this backwards dropped every document
  // line and kept only the buttons; the unit test caught it.
  const CONTAINER = /^(Pane|Window|Group|Custom|TitleBar|MenuBar|ToolBar|StatusBar|ScrollBar|Header|HeaderItem)$/
  const KEEP_ROLE = /^(Button|Edit|RadioButton|CheckBox|Hyperlink|ListItem|TabItem|MenuItem|ComboBox|TreeItem|Slider|Spinner|Document|Text|Image)$/
  let treeHeader = ''
  for (const raw of lines) {
    const line = raw.replace(/\s+$/, '')
    if (!line.trim()) continue
    if (/^===\s*UIA fallback tree/.test(line) || /^These nodes have NO helper element_index/.test(line)) {
      treeHeader = '=== UIA tree (summary: document text + interactive nodes; no container nesting) ==='
      continue
    }
    const m = /^\s*(?:(\d+)\s+)?([A-Z][A-Za-z]*)\s+(.*)$/.exec(line)
    if (!m) {
      // Bare text rows (document text blobs) are exactly what a reader needs — keep them.
      keep.push(line.trim())
      continue
    }
    const role = m[2]
    if (CONTAINER.test(role)) continue          // pure container: no information of its own
    // Keep the role tag: the model picks elements by role+name+rect, and dropping the role
    // made a RadioButton indistinguishable from a bare Text row (caught by unit test).
    if (KEEP_ROLE.test(role)) { keep.push(role + ' ' + m[3]); continue }
    // Unknown role: keep only when it carries a name that is not just a container label.
    if (m[3] && !/^\{\{x:/.test(m[3])) keep.push(role + ' ' + m[3])
  }
  const deduped = [...new Set(keep)]
  return [treeHeader || '=== UIA tree (summary) ===', ...deduped].join('\n')
}

/** The sentence that explains a degraded capture, including an oversize window (P2-7). */
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

/** Extract live document URLs from a .NET UIA node list (Document role). */
export function collectDocumentUrls(nodes) {
  const seen = new Set()
  const out = []
  for (const node of Array.isArray(nodes) ? nodes : []) {
    if (!node) continue
    const role = String(node.role || '').toLowerCase()
    const value = String(node.value || '')
    if ((role === 'document' || role === '文档') && /^https?:\/\//i.test(value) && !seen.has(value)) {
      seen.add(value)
      out.push({ url: value, title: String(node.name || '') })
    }
  }
  return out
}

/** Shared sequential-scroll loop for scroll_to / reach_last_page. */
/**
 * Read a "current/total" page indicator out of a window's UIA text.
 *
 * `scroll_to` / `reach_last_page` are wheel-driven, but wheel is not universal: the
 * moxueyuan image gallery (`/image/preview?...img-1.jpg…img-10.jpg`) ignores it
 * completely — 52 pulses ran and the indicator stayed at `4/10`. A page counter is the
 * cheap universal signal that paging has NOT happened, and it is present in exactly
 * those viewers (PDF `2/13`, gallery `4/10`, slide decks).
 *
 * Candidates inside a longer slash run (a URL such as `…/2026/01/12/…`) are rejected:
 * the match must not be preceded or followed by a digit or another slash.
 *
 * @param {string} text any text (usually the UIA fallback tree dump)
 * @returns {{current: number, total: number}|null} the widest plausible indicator
 */
export function parsePageIndicator(text) {
  const src = String(text || '')
  let best = null
  for (const m of src.matchAll(/(^|[^\d/])(\d{1,3})\s*\/\s*(\d{1,3})(?![\d/])/g)) {
    const current = Number(m[2])
    const total = Number(m[3])
    if (!Number.isFinite(current) || !Number.isFinite(total)) continue
    if (total < 2 || total > 999 || current < 1 || current > total) continue
    if (!best || total > best.total) best = { current, total }
  }
  return best
}

/** Ask the window for its page indicator via the .NET UIA walk (best effort, ~150 ms). */
export function readPageIndicator(hwnd) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return null
  try {
    const nodes = uiaTreeFallback(hwnd, 600)
    if (!Array.isArray(nodes) || nodes.length === 0) return null
    const text = nodes.map(n => [n.name, n.value].filter(Boolean).join(' ')).join('\n')
    return parsePageIndicator(text)
  } catch {
    return null
  }
}

/**
 * Drive pagination with keys when the wheel did not move the page counter.
 *
 * Tries the document-friendly key first, then the arrow key most galleries bind, and
 * re-reads the indicator after every press so it stops as soon as the counter reaches
 * the last page (or the viewer stops responding).
 *
 * @returns {Promise<{from: number, to: number, total: number, key: string|null}>}
 */
async function pageThroughByKeys(ctx, win, start, exec, turnMeta, probe, opts = {}) {
  const hwnd = Number(win && win.id)
  const out = { from: start.current, to: start.current, total: start.total, key: null }
  const gap = () => new Promise((resolve) => setTimeout(resolve, 90))
  const budget = () => Math.min(start.total - start.current + 1, 40)
  for (const key of ['Page_Down', 'Right', 'Next']) {
    let cur = out.to
    let moved = 0
    for (let i = 0; i < budget(); i += 1) {
      try {
        await ctx.dshComputerUse.call('press_key', { window: win, key }, exec.signal, turnMeta)
      } catch {
        break
      }
      await gap()
      const now = probe(hwnd)
      if (!now) break
      if (now.current > cur) {
        moved += 1
        cur = now.current
        out.key = key
        out.to = cur
      }
      if (now.current >= now.total) break
    }
    if (moved > 0 || out.to >= out.total) return out
  }
  // Last resort: a horizontal SWIPE. Measured on the moxueyuan image gallery, the wheel
  // and Page_Down/Right/Next are all ignored, but a left drag advances exactly one image
  // (2/10 -> 3/10). Galleries and carousels are drag-driven, not keyboard-driven.
  const w = Number(opts.width) > 0 ? Number(opts.width) : 784
  const h = Number(opts.height) > 0 ? Number(opts.height) : 1628
  const cy = Number.isFinite(opts.y) ? opts.y : Math.round(h / 2)
  const forward = opts.direction !== 'top'
  const leftX = Math.round(w * 0.78)
  const rightX = Math.round(w * 0.2)
  const fromX = forward ? leftX : rightX
  const toX = forward ? rightX : leftX
  let cur = out.to
  for (let i = 0; i < budget(); i += 1) {
    // A drag is a coordinate action: it needs the lease, and every other swipe re-clears
    // the input monitor so one un-taggable event cannot poison the rest of the run.
    if (i % 2 === 1) {
      try { await ctx.dshComputerUse.call('clear_user_input', {}, exec.signal, turnMeta) } catch { /* older helper */ }
    }
    try {
      // Interpolated SendInput swipe first (no lease, real pointer motion); fall back to
      // the helper's 4-event drag only when the script itself failed.
      const viaScript = swipeViaScript(hwnd, { fromX, fromY: cy, toX, toY: cy })
      if (!viaScript || viaScript.ok !== true) {
        await ctx.dshComputerUse.call(
          'drag',
          { window: win, from_x: fromX, from_y: cy, to_x: toX, to_y: cy },
          exec.signal,
          turnMeta,
        )
      }
    } catch {
      break
    }
    await gap()
    const now = probe(hwnd)
    if (!now) break
    const advanced = forward ? now.current > cur : now.current < cur
    if (advanced) {
      cur = now.current
      out.key = 'swipe'
      out.to = cur
    }
    const done = forward ? now.current >= now.total : now.current <= 1
    if (done) break
  }
  return out
}

export async function runScrollTo(ctx, state, args, exec, defaultSteps, probe = readPageIndicator) {
  const win = args && args.window
  const percent = args && args.percent !== undefined ? Math.min(Math.max(Number(args.percent) || 0, 0), 100) : null
  const to = percent !== null ? (percent >= 50 ? 'bottom' : 'top') : (args.to === 'top' ? 'top' : 'bottom')
  const steps = Math.min(Math.max(Math.trunc(args.steps ?? defaultSteps), 1), 80)
  const perStep = Math.min(Math.max(Math.trunc(args.delta ?? 12000), 200), 60000) * (to === 'top' ? -1 : 1)
  const meta = state && state.lastShotMeta
  let x = Number.isFinite(args.x) ? args.x : Math.round(((meta && meta.width) || 800) / 2) + (Number.isFinite(meta && meta.originX) ? meta.originX : 0)
  let y = Number.isFinite(args.y) ? args.y : Math.round(((meta && meta.height) || 600) / 2) + (Number.isFinite(meta && meta.originY) ? meta.originY : 0)
  const turnMeta = turnMetaFor(ctx.dshComputerUse, exec)
  let done = 0
  const sendScroll = () => ctx.dshComputerUse.call('scroll', { window: win, x, y, scrollX: 0, scrollY: perStep }, exec.signal, turnMeta)
  for (let i = 0; i < steps; i += 1) {
    let r
    try {
      r = await sendScroll()
    } catch (err) {
      const msg = String(err && err.message ? err.message : err)
      // P1-4/P1-5: the human-input monitor or an empty lease refuses the wheel before it
      // runs. The scroll never happened, so un-wedge and retry once:
      // clear_user_input clears the monitor without a capture (the helper may not be
      // able to produce one on a static/oversize/covered window), and the observation
      // that follows satisfies the "call get_window_state before continuing" contract.
      const unwedgeable = /user input was detected|needs a screenshot from the last observation|none is cached|not the latest observation|bounds changed before coordinate input|window bounds changed/i.test(msg)
      if (unwedgeable) {
        try { await ctx.dshComputerUse.call('clear_user_input', {}, exec.signal, turnMeta) } catch { /* older helper: no such method */ }
        try {
          const observed = await ctx.dshComputerUse.call(
            'get_window_state',
            { window: win, include_screenshot: true, include_text: false },
            exec.signal,
            turnMeta,
          )
          if (observed && Array.isArray(observed.value?.screenshots)) externalizeScreenshotIds(state, observed.value)
        } catch { /* the retry below is what reports success or failure */ }
        r = await sendScroll()
      } else {
        // The cached screenshot geometry can predate a resize/maximise (stale centre →
        // "point … is outside viewport { originX, originY, width, height }"). The error
        // carries the live viewport, so re-centre from it and retry once.
        const m = /outside viewport \{\s*originX:\s*(-?[\d.]+),\s*originY:\s*(-?[\d.]+),\s*width:\s*([\d.]+),\s*height:\s*([\d.]+)/.exec(msg)
        if (!m) throw err
        const [ox, oy, vw, vh] = m.slice(1, 5).map(Number)
        x = Math.round(ox + vw / 2)
        y = Math.round(oy + vh / 2)
        if (state) state.lastShotMeta = { ...(state.lastShotMeta || {}), originX: ox, originY: oy, width: vw, height: vh }
        r = await sendScroll()
      }
    }
    if (!r || r.ok === false) break
    done += 1
    // Space out pulses: many apps coalesce rapid wheel events into a single scroll, so a
    // tight loop only moves a handful of pages. A short gap lets each pulse register.
    if (i + 1 < steps) await new Promise((resolve) => setTimeout(resolve, 70))
  }
  const result = { tool: 'computer_use_scroll_to', to, percent, stepsDone: done, perStep, at: { x, y } }
  // Wheel-only paging is not universal (see parsePageIndicator): the moxueyuan image
  // gallery ignored 52 pulses and stayed at 4/10. If the window exposes a page counter
  // and the wheel left it short of the last page, drive the pager with keys instead.
  const hwnd = Number(win && win.id)
  const atTargetEnd = (ind) => (to === 'bottom' ? ind.current >= ind.total : ind.current <= 1)
  if (done >= 2 && Number.isInteger(hwnd)) {
    let ind = null
    try { ind = probe(hwnd) } catch { ind = null }
    if (ind && !atTargetEnd(ind)) {
      // Swiping needs a live capture lease AND the window's CURRENT rectangle: the cached
      // `lastShotMeta` can predate a resize (784x1628 -> 1668x744), which puts the swipe
      // endpoints outside the window and makes every drag fail silently.
      let fresh = null
      try {
        const observed = await ctx.dshComputerUse.call(
          'get_window_state',
          { window: win, include_screenshot: true, include_text: false },
          exec.signal,
          turnMeta,
        )
        const shot = observed && observed.value && Array.isArray(observed.value.screenshots)
          ? observed.value.screenshots[0]
          : null
        if (shot && shot.width && shot.height) {
          fresh = { width: shot.width, height: shot.height }
          if (state) state.lastShotMeta = { ...(state.lastShotMeta || {}), width: shot.width, height: shot.height }
        }
      } catch { /* paging still tries; the swipe loop reports the stall */ }
      const paged = await pageThroughByKeys(ctx, win, ind, exec, turnMeta, probe, {
        width: fresh && fresh.width,
        height: fresh && fresh.height,
        y,
        direction: to,
      })
      result.page = { ...paged, key: paged.key || 'none', wheelStoppedAt: ind.current }
      result.stepsDoneByKey = to === 'bottom' ? paged.to - paged.from : paged.from - paged.to
      if (paged.to === paged.from) {
        // The counter is readable and NOT at the last page, yet the wheel, the three key
        // strategies and the interpolated swipe all left it where it was. Say that out
        // loud instead of returning `key:null` with no verdict (that is the same silent
        // failure this tool was fixed for, just reached through the counter branch).
        result.moved = 'none'
        result.swipe = { tried: true, pageStayedAt: ind.current }
        result.guidance = 'The page counter stayed at ' + ind.current + '/' + ind.total + ' after the wheel, '
          + 'Page_Down/Right/Next and an interpolated swipe, so this viewer binds none of those gestures. '
          + 'Next step: computer_use_uia_act on its next-page/下一题 control (read the tree first — one '
          + 'WeChatAppEx window can hold several courses, and its first Document node is often a hidden one).'
      }
    } else if (ind) {
      result.page = { from: ind.current, to: ind.current, total: ind.total, key: 'none', wheelStoppedAt: ind.current }
      result.atEnd = atTargetEnd(ind)
    } else {
      // No page counter at all (the UIA walk failed, or the page has none). Staying
      // silent here made "never even tried" look identical to "tried and failed".
      //
      // Three signals, strongest first: a text diff of the UIA tree (a slide reader's
      // document text carries "1/13" and the completion marker), then a frame hash, and
      // when neither moved we say UNKNOWN rather than false — GDI PrintWindow happily
      // returns a stale frame for a Chromium surface, so "same pixels" is not proof.
      const before = documentSnapshot(hwnd)
      const hashBefore = frameHash(hwnd)
      const geo = state && state.lastShotMeta ? state.lastShotMeta : null
      const w = Number(geo && geo.width) > 0 ? Number(geo.width) : 784
      const h = Number(geo && geo.height) > 0 ? Number(geo.height) : 1628
      const doc = documentRect(hwnd)
      // A WebView2 window can hold SEVERAL documents (a hidden tab from an earlier
      // course). Only a rect that fits this window is the one on screen — measured: the
      // first Document line belonged to a 768x1564 portrait document inside a 1668x744
      // window, and swiping there would land off-window.
      const box = doc
        && doc.x >= -8 && doc.y >= -8
        && doc.x + doc.width <= w + 24 && doc.y + doc.height <= h + 24
        ? doc
        : null
      const left = box ? box.x : 0
      const right = box ? box.x + box.width : w
      const cy = box ? Math.round(box.y + box.height / 2) : (Number.isFinite(y) ? y : Math.round(h / 2))
      const swiped = swipeViaScript(hwnd, {
        fromX: Math.round(left + (right - left) * 0.78),
        fromY: cy,
        toX: Math.round(left + (right - left) * 0.2),
        toY: cy,
      })
      const after = documentSnapshot(hwnd)
      const hashAfter = frameHash(hwnd)
      const textMoved = Boolean(before && after && before.text !== after.text)
      const frameMoved = Boolean(hashBefore && hashAfter && hashBefore !== hashAfter)
      result.page = null
      result.pageProbe = 'unavailable'
      result.swipe = { ok: Boolean(swiped && swiped.ok === true), from: box ? 'document-rect' : 'window-rect' }
      result.moved = textMoved ? 'text' : (frameMoved ? 'frame' : 'unknown')
      result.lastPage = Boolean(after && after.lastPage)
      result.guidance = 'No "current/total" page counter could be read for this window, so paging cannot be measured. '
        + 'One interpolated swipe was issued inside the visible document rectangle: '
        + (textMoved
          ? 'the accessibility text CHANGED, so the gesture landed — call reach_last_page again to continue.'
          : frameMoved
            ? 'the frame changed but the text did not — possibly a transient paint; re-read the page before trusting it.'
            : 'nothing observable changed. GDI frames can be stale on Chromium, so this is UNKNOWN rather than a '
              + 'proven no-op; if it really did not move, the viewer needs a click on its next control.')
        + (result.lastPage ? ' The document already reports its LAST page.' : '')
    }
  }
  return result
}

export function scrollToTool(ctx, state) {
  return defineTool({
    name: 'computer_use_scroll_to',
    description:
      'Scroll a window to the bottom (default) or top, or toward an approximate percent, in ONE call by issuing '
      + 'several SEQUENTIAL wheel scrolls. This replaces the "one scroll per round-trip" dance and avoids the batch '
      + 'coalescing that makes many parallel scrolls a no-op. Pass x/y inside the scrollable pane.',
    parameters: {
      window: WIN_PARAM,
      to: { type: 'string', enum: ['bottom', 'top'], description: 'Direction (default bottom).' },
      percent: { type: 'integer', description: 'Approximate target percent 0-100; >=50 scrolls down, <50 up.' },
      steps: { type: 'integer', description: 'Max scroll pulses (default 12, clamp 1..80).' },
      delta: { type: 'integer', description: 'Pixels per pulse (default 12000, clamp 200..60000).' },
      x: { type: 'number', description: 'X inside the scroll pane (default window centre).' },
      y: { type: 'number', description: 'Y inside the scroll pane (default window centre).' },
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args, exec) {
      return { value: await runScrollTo(ctx, state, args && typeof args === 'object' ? args : {}, exec, 12) }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_scroll_to', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

export function reachLastPageTool(ctx, state) {
  return defineTool({
    name: 'computer_use_reach_last_page',
    description:
      'Scroll a long document to its LAST page in one call (a computer_use_scroll_to(bottom) with a bigger default pulse budget). '
      + 'For readers/viewers that only count completion when the last page is reached.',
    parameters: {
      window: WIN_PARAM,
      steps: { type: 'integer', description: 'Max scroll pulses (default 40, clamp 1..80).' },
      delta: { type: 'integer', description: 'Pixels per pulse (default 40000).' },
      x: { type: 'number', description: 'X inside the scroll pane (default window centre).' },
      y: { type: 'number', description: 'Y inside the scroll pane (default window centre).' },
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args, exec) {
      const a = args && typeof args === 'object' ? { ...args, to: 'bottom' } : { to: 'bottom' }
      if (a.delta === undefined) a.delta = 40000
      return { value: await runScrollTo(ctx, state, a, exec, 40) }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_reach_last_page', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

export function waitUntilTool(ctx, state) {
  return defineTool({
    name: 'computer_use_wait_until',
    description:
      'Poll a window until a condition is met instead of a blind fixed sleep: waits until `text` appears in the '
      + 'accessibility tree, or until the screenshot `change`s from the first frame. Returns {satisfied, reason, waitedMs, polls}.',
    parameters: {
      window: WIN_PARAM,
      text: { type: 'string', description: 'Wait until this text appears in the accessibility tree / UIA fallback.' },
      change: { type: 'boolean', description: 'Wait until the screenshot differs from the first captured frame.' },
      timeoutMs: { type: 'integer', description: 'Max wait (default 15000, clamp 0..120000).' },
      intervalMs: { type: 'integer', description: 'Poll interval (default 700, clamp 100..5000).' },
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args, exec) {
      const win = args && args.window
      const wantText = typeof args?.text === 'string' && args.text.length > 0
      const wantChange = Boolean(args?.change)
      if (!wantText && !wantChange) return { value: { satisfied: false, reason: 'no-condition', waitedMs: 0, polls: 0 } }
      const timeout = Math.min(Math.max(Math.trunc(args.timeoutMs ?? 15_000), 0), 120_000)
      const interval = Math.min(Math.max(Math.trunc(args.intervalMs ?? 700), 100), 5_000)
      const chromium = looksLikeChromiumHost(win)
      const turnMeta = turnMetaFor(ctx.dshComputerUse, exec)
      const start = Date.now()
      let polls = 0
      let baseHash = null
      while (true) {
        polls += 1
        let val = null
        if (wantChange) {
          // Need a screenshot hash; text (if also asked) comes from the fallback for
          // Chromium hosts, so only ask the helper for text on non-Chromium windows.
          const res = await observeWithLadder(
            ctx, state, exec, turnMeta, win,
            { include_text: wantText && !chromium },
          )
          val = res && res.value !== undefined ? res.value : res
          const img = (res && Array.isArray(res.images) ? res.images : [])[0]
          if (img && img.data) {
            const h = crypto.createHash('sha1').update(String(img.data)).digest('hex')
            if (baseHash === null) baseHash = h
            else if (h !== baseHash) return { value: { satisfied: true, reason: 'screen-changed', waitedMs: Date.now() - start, polls } }
          }
        } else if (wantText && !chromium) {
          // Non-Chromium: ask the helper for its accessibility tree directly.
          const res = await ctx.dshComputerUse.call(
            'get_window_state',
            { window: win, include_screenshot: false, include_text: true },
            exec.signal, turnMeta,
          )
          val = res && res.value !== undefined ? res.value : res
        }
        // (Chromium text-only: skip get_window_state entirely and read the .NET fallback.)
        if (wantText) {
          let tree = val && val.accessibility && typeof val.accessibility.tree === 'string' ? val.accessibility.tree : ''
          if (!tree && chromium) {
            const fb = uiaTreeFallback(Number(win && win.id))
            if (Array.isArray(fb) && fb.length) tree = formatUiaFallback(fb)
          }
          if (tree && tree.includes(args.text)) return { value: { satisfied: true, reason: 'text-appeared', waitedMs: Date.now() - start, polls } }
        }
        if (Date.now() - start >= timeout) return { value: { satisfied: false, reason: 'timeout', waitedMs: Date.now() - start, polls } }
        await sleepRespectingSignal(interval, exec.signal)
      }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_wait_until', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

export function documentsTool() {
  return defineTool({
    name: 'computer_use_documents',
    description:
      'List live document URLs exposed by a window via UIA (Document role ValuePattern). For PDF/viewer courseware this '
      + 'returns the resource URL so the content can be fetched/read directly instead of scrolling page by page. Read-only.',
    parameters: {
      window: WIN_PARAM,
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args) {
      const win = args && args.window
      const nodes = uiaTreeFallback(Number(win && win.id)) || []
      return { value: { documentUrls: collectDocumentUrls(nodes), nodes: Array.isArray(nodes) ? nodes.length : 0 } }
    },
    isConcurrencySafe() { return true },
    presentCall() { return { card: 'generic', title: 'computer_use_documents' } },
  })
}

// ---------------------------------------------------------------------------
// UIA tree semantic compression + dead-tree detection.
//
// A Chromium/WebView2/mini-program (WMPF) window publishes only a chain of
// anonymous containers over UIA: no names, no roles, no values. Printing that
// tree costs thousands of tokens and tells the model nothing. Two levers:
//   1. compress: drop decorative leaves and keep only meaningful nodes plus the
//      ancestors that hold them (indices are preserved, so element_index stays valid).
//   2. detect dead: if the tree is (almost) all anonymous containers, stop
//      pretending it is usable and emit one explicit "this path is dead" verdict.
// ---------------------------------------------------------------------------

// Roles that carry no information on their own (their *name* would have to).
const GENERIC_CONTAINER_ROLES = new Set([
  'pane', '窗格', 'group', '组', 'custom', '自定义', 'region', '区域',
  'client', '客户端', 'unknown', '未知', 'document', '文档',
])
// Roles that are inherently interactive/informative even with no extra text.
const MEANINGFUL_ROLES = new Set([
  'button', '按钮', 'edit', '编辑', 'menuitem', '菜单项', 'link', '链接', 'hyperlink', '超链接',
  'checkbox', '复选框', 'radiobutton', '单选按钮', 'combobox', '组合框', 'list', '列表',
  'listitem', '列表项', 'tab', '选项卡', 'tabitem', 'text', '文本', 'image', '图像',
  'slider', '滑块', 'spinner', '微调按钮', 'treeitem', '树项目', 'menuitemcheckbox',
])
const _TREE_EXTRA_MARKERS = ['Description: ', 'Value: ', 'Secondary Actions: ', 'ID: ']

function parseTreeLine(line) {
  const match = /^(\t*)(\d+)(?: +([^\s]+))?(.*)$/.exec(line)
  if (!match) return null
  return { indent: match[1].length, index: Number(match[2]), role: (match[3] || '').toLowerCase(), rest: (match[4] || ''), raw: line }
}

/** Does this node carry information (a name, a value/description, or a real role)? */
function nodeIsMeaningful(node) {
  if (!node) return false
  if (MEANINGFUL_ROLES.has(node.role)) return true
  if (_TREE_EXTRA_MARKERS.some(marker => node.rest.includes(marker))) return true
  // A name: strip a leading parenthesised state block, then any trailing extras.
  let text = node.rest.trim()
  const close = text.startsWith('(') ? text.indexOf(')') : -1
  if (close !== -1) text = text.slice(close + 1).trim()
  for (const marker of _TREE_EXTRA_MARKERS) {
    const at = text.indexOf(marker)
    if (at !== -1) text = text.slice(0, at).trim()
  }
  text = text.replace(/(?:^| )(off|on|indeterminate)$/, '').trim()
  if (text) return true
  // A non-generic role with no name still hints at structure worth keeping.
  return Boolean(node.role) && !GENERIC_CONTAINER_ROLES.has(node.role)
}

/** Does this node carry actual *content* (a live value/description, not just a label)? */
// Window-chrome roles carry labels but never page content; their `Value:` (e.g. the
// title bar's window title) must NOT be mistaken for content, otherwise it masks a
// genuinely useless/anonymous content tree and blocks the .NET fallback.
const CHROME_ROLES = new Set(['titlebar', '标题栏', 'menubar', '菜单栏', 'statusbar', '状态栏'])

function nodeHasContent(node) {
  if (!node) return false
  if (CHROME_ROLES.has(node.role)) return false
  if (node.rest.includes('Value: ') || node.rest.includes('Description: ')) return true
  // A text/document role that carries a readable name is content too.
  return (node.role === 'text' || node.role === '文本' || node.role === '文档') && nodeIsMeaningful(node)
}

/** Is this window's process a known Chromium/WebView2/Electron/WMPF host? (app string only, no shell) */
export function looksLikeChromiumHost(window) {
  const app = String(window?.app || '')
  if (!app) return false
  return /WeChatAppEx|RadiumWMPF|wmpf|msedgewebview2|webview2|electron|chrome\.exe|chromium|msedge|Microsoft\.Edge|Code\.exe|slack|discord|teams|notion|obsidian/i.test(app)
}

export function classifyAccessibilityTree(treeText, window) {
  const lines = String(treeText || '').split('\n')
  let total = 0
  let meaningful = 0
  let contentBearing = 0
  for (const line of lines) {
    const node = parseTreeLine(line)
    if (!node) continue
    total += 1
    if (nodeIsMeaningful(node)) meaningful += 1
    if (nodeHasContent(node)) contentBearing += 1
  }
  const ratio = total === 0 ? 1 : meaningful / total
  // "Dead" means: enough nodes to be a real tree, but NOT ONE content-bearing node —
  // and either the process is a known Chromium/WebView2/WMPF host (which renders into
  // a canvas and publishes only window chrome over UIA) or literally nothing is
  // meaningful. Window-chrome buttons (minimize/close) do NOT count as content, so they
  // no longer mask a useless tree.
  const dead = total >= 8 && contentBearing === 0 && (looksLikeChromiumHost(window) || meaningful <= 1)
  return { total, meaningful, contentBearing, ratio, dead }
}

/**
 * Drop decorative leaves while keeping every meaningful node and the ancestor
 * containers that lead to it. Node indices are untouched, so element_index from
 * the helper still resolves against the retained lines.
 */
export function compressAccessibilityTree(treeText) {
  const lines = String(treeText || '').split('\n')
  const nodes = []
  const nodeLineIndexes = []
  lines.forEach((line, index) => {
    const node = parseTreeLine(line)
    if (node) { nodes.push({ ...node, keep: nodeIsMeaningful(node) }); nodeLineIndexes.push(index) }
  })
  if (nodes.length === 0) return treeText
  // Propagate "has a meaningful descendant" up through the indent stack.
  const stack = []
  for (const node of nodes) {
    while (stack.length && stack[stack.length - 1].indent >= node.indent) stack.pop()
    if (node.keep) for (const ancestor of stack) ancestor.hasMeaningfulDescendant = true
    stack.push(node)
  }
  const kept = nodes.filter(node => node.keep || node.hasMeaningfulDescendant)
  if (kept.length === 0) return treeText
  const dropped = nodes.length - kept.length
  const firstNodeLine = nodeLineIndexes[0]
  const lastNodeLine = nodeLineIndexes[nodeLineIndexes.length - 1]
  const header = lines.slice(0, firstNodeLine)
  const footer = lines.slice(lastNodeLine + 1)
  const out = [...header, ...kept.map(node => node.raw), ...footer].join('\n')
  return dropped > 0
    ? `${out}\n# (semantic compression: ${dropped} anonymous container node(s) omitted; indices above are unchanged)`
    : out
}

/** One explicit verdict that replaces a useless tree. */
export function deadTreeSignal(classification, cdp) {
  const lines = [
    '=== Accessibility tree is UNUSABLE for this window (semantic verdict) ===',
    `nodes: ${classification.total}, meaningful labels: ${classification.meaningful}, content-bearing: ${classification.contentBearing} — no window content is exposed over UIA, only anonymous containers and window chrome.`,
    'This window is a Chromium/WebView2 or mini-program (WeChat WMPF/Radium) surface that does not publish its DOM over UIA.',
    'Do NOT call get_window_state with include_text for this window again (it costs tokens and yields nothing).',
    'Use screenshots + vision (vision_map / vision_read / vision_zoom) for content, and act by x/y coordinates; element_index / set_value / perform_secondary_action will not work here.',
  ]
  if (cdp) {
    if (cdp.canAttach && cdp.endpoints?.length) {
      const port = cdp.endpoints[0].port
      lines.push(`CDP IS reachable on 127.0.0.1:${port} (${cdp.endpoints[0].browser || 'chromium'}); prefer DOM automation: attach chrome-devtools MCP with --browserUrl http://127.0.0.1:${port}.`)
    } else if (cdp.chromiumLike) {
      lines.push(`CDP: ${cdp.guidance}`)
    }
  }
  return lines.join('\n')
}

/** Cache a CDP probe per app so a dead-tree window does not spawn a shell on every observation. */
async function cachedCdpProbe(state, window, maxAgeMs = 60_000) {
  if (!window || typeof window.app !== 'string' || !window.app) return null
  state.cdpProbeCache = state.cdpProbeCache || new Map()
  const key = window.app
  const hit = state.cdpProbeCache.get(key)
  const now = Date.now()
  if (hit && now - hit.at < maxAgeMs) return hit.value
  let value = null
  try { value = await probeCdp({ window }) } catch { value = null }
  state.cdpProbeCache.set(key, { at: now, value })
  return value
}

const UIA_TREE_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'scripts', 'uia-tree.ps1')
const UIA_ACT_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'scripts', 'uia-act.ps1')

/** Render a .NET-UIА fallback node list into a readable tree with window-relative bounds. */
export function formatUiaFallback(nodes) {
  const lines = [
    '=== UIA fallback tree (.NET UIAutomation ControlView) ===',
    'The helper-provided tree under-reported this Chromium/WebView2 window; this is a fuller walk of the same window.',
    'These nodes have NO helper element_index. Act by x/y using the window-relative bounds shown as {{x,y,w,h}}.',
    '',
  ]
  for (const n of nodes) {
    const indent = '\t'.repeat(Math.min((n.depth || 0) + 1, 12))
    const role = n.role || 'Node'
    const name = n.name ? ` ${n.name}` : ''
    const value = n.value ? ` Value: ${n.value}` : ''
    const box = ` {{x:${n.x | 0},y:${n.y | 0},w:${n.w | 0},h:${n.h | 0}}}`
    lines.push(`${indent}${role}${name}${value}${box}`)
  }
  return lines.join('\n')
}

/**
 * Walk a window's UIA tree through the bundled .NET script and return nodes with
 * window-relative bounds. This is the fix for Chromium/WebView2/mini-program windows
 * (WeChat WMPF, Electron, WebView2) whose tree the native helper under-reports down to
 * anonymous panes: a .NET UIAutomation ControlView walk DOES reach the Document/Text/
 * ListItem content and the Document value carries the live URL. Returns null on failure.
 */
export function uiaTreeFallback(hwnd, maxNodes = 800) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return null
  try {
    const raw = execFileSync(
      'powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', UIA_TREE_SCRIPT, '-Hwnd', String(hwnd), '-MaxNodes', String(maxNodes)],
      { encoding: 'utf8', timeout: 20_000, windowsHide: true, maxBuffer: 16 * 1024 * 1024 },
    ).trim()
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : [parsed]
  } catch {
    return null
  }
}

/**
 * Drive a control through a UIA pattern instead of a coordinate click.
 *
 * Windows Graphics Capture only produces a frame when the window repaints, so a
 * static page (exam/form) cannot be screenshot, and coordinate input is refused.
 * The same .NET UIAutomation walk used for the text fallback can Select/Invoke
 * controls directly, which is what these H5 pages need.
 */
export function uiaActFallback(hwnd, name, action) {
  if (!Number.isInteger(hwnd) || hwnd <= 0 || !name) return { ok: false, error: 'bad arguments' }
  try {
    const raw = execFileSync(
      'powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', UIA_ACT_SCRIPT,
        '-Hwnd', String(hwnd), '-Name', String(name), '-Action', String(action || 'auto')],
      { encoding: 'utf8', timeout: 25_000, windowsHide: true, maxBuffer: 8 * 1024 * 1024 },
    ).trim()
    const line = raw.split(/\r?\n/).find((l) => l.trim().startsWith('{'))
    return line ? JSON.parse(line) : { ok: false, error: raw || 'no output' }
  } catch (err) {
    // The script prints its verdict and THEN exits 2/3, so the payload is on stdout;
    // err.message alone is `Command failed: powershell.exe …` and loses the reason.
    const parsed = parseJsonLine(err && err.stdout)
    if (parsed) return parsed
    return { ok: false, error: String(err && err.message ? err.message : err) }
  }
}

/**
 * Decide one act step's verdict from the page fingerprint before/after.
 *
 * Split out as a pure function because the rule is the whole point of P1-5 and must be
 * pinned by a test: uia_act's `ok` only ever meant "the click was issued", so a WebView2
 * option that silently ignored the click still came back ok and a multi-select page looked
 * answered when nothing was ticked.
 *
 * @returns {{applied?: boolean, unchanged?: boolean, hint?: string}} empty when unverifiable
 */
export function judgeActStep(before, after, ok) {
  if (!ok) return {}
  if (!before || !after) return {}      // cannot read the page: no claim either way
  if (before === after) {
    return {
      applied: false,
      unchanged: true,
      hint: 'the click was issued but the page did not change — verify the effect '
        + '(on a WebView2/WMPF page a bare Text option can ignore the click entirely)',
    }
  }
  return { applied: true }
}

// ---------------------------------------------------------------------------
// Persistent-UIA options tools (P0-2/3/4).
//
// These go through scripts/uiahost.ps1 (see src/uiahost-client.js) rather than the
// helper, because the work they do — locating a checkbox by scanning pixels and reading
// its tick state — is neither a UIA pattern nor a coordinate click the helper can verify.
// Keeping a PowerShell process alive turns the old 1.5-2 s per read into ~180 ms.
// ---------------------------------------------------------------------------

/** `computer_use_read_options` — question + per-option {letter, text, checked, x, y}. */
export function readOptionsTool(ctx, state) {
  return defineTool({
    name: 'computer_use_read_options',
    description:
      'Read a multiple-choice page: returns the question text and, for each option, its letter, label, '
      + 'whether it is TICKED, and the real checkbox position. Tick state is read from PIXELS (a tick is a '
      + 'red square), so it works on WebView2/WMPF pages whose options expose no UIA pattern at all — the '
      + 'one place where neither element names nor a read of the tree can tell you what is selected. '
      + 'Runs against the persistent UIA host (≈180 ms, vs 1.5-2 s for a fresh process).',
    parameters: { window: WIN_PARAM },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args) {
      const win = args && args.window
      const hwnd = Number(win && win.id)
      if (!Number.isInteger(hwnd) || hwnd <= 0) throw new Error('window.id is required')
      const host = await getUiaHostReady()
      const res = await host.read(hwnd)
      if (res.ok !== true) throw new Error(String(res.error || 'could not read the page'))
      return {
        value: {
          question: String(res.question || ''),
          options: Array.isArray(res.options) ? res.options : [],
          ticked: String(res.ticked || ''),
          ms: res.ms,
        },
      }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_read_options', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

/** `computer_use_set_options` — make the ticked set exactly `target`, verified by re-reading. */
export function setOptionsTool(ctx, state) {
  return defineTool({
    name: 'computer_use_set_options',
    description:
      'Make a multiple-choice question\'s ticked options EXACTLY the letters in `target` (e.g. "ACDE"), '
      + 'then verify by re-reading every tick from pixels. It clicks ONE differing option per round and '
      + 're-reads the row positions each time, because these pages reflow after every tick — batching a set '
      + 'of coordinates is what makes this class of page silently wrong. Returns '
      + '{before, after, target, matched} so success is a fact, not an assumption.',
    parameters: {
      window: WIN_PARAM,
      target: { type: 'string', description: 'The exact set of letters that should be ticked, e.g. "ACDE" (no separators). Use "" to clear all.' },
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args) {
      const win = args && args.window
      const hwnd = Number(win && win.id)
      if (!Number.isInteger(hwnd) || hwnd <= 0) throw new Error('window.id is required')
      const target = String((args && args.target) || '').toUpperCase().replace(/[^A-E]/g, '')
      const host = await getUiaHostReady()
      const res = await host.set(hwnd, target)
      if (res.ok !== true) throw new Error(String(res.error || 'could not set the options'))
      return {
        value: {
          question: String(res.question || ''),
          before: String(res.before || ''),
          after: String(res.after || ''),
          target: String(res.target || target),
          matched: res.matched === true,
          rounds: res.rounds,
          log: Array.isArray(res.log) ? res.log : [],
          ms: res.ms,
        },
      }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_set_options', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

/** `computer_use_uia_act` — act by element name (UIA patterns, then a click fallback), then optionally re-observe. */
export function uiaActTool(ctx, state) {
  return defineTool({
    name: 'computer_use_uia_act',
    description:
      'Act on this window by ELEMENT NAME using UIA patterns (Select/Invoke), with a coordinate-click fallback for '
      + 'nodes that expose no pattern at all (H5 multi-select options and plain `Text` buttons such as 交卷) — no '
      + 'screenshot needed, so it works on static windows that Windows Graphics Capture cannot frame (exam/form pages '
      + 'in WeChat/WebView2). Pass several names separated by "|", or an array in `steps`, to run them in order '
      + '(e.g. "A、选项文本|下一题", or steps:["A","B","D"] to tick a multi-select). '
      + 'Returns the resulting text tree so the next step can be planned in the same call.',
    parameters: {
      window: WIN_PARAM,
      names: { type: 'string', description: 'Element name(s); a distinctive substring is enough. Separate several steps with "|".' },
      steps: {
        type: 'array',
        items: { type: 'string' },
        description: 'Multi-value form helper: one label per entry (e.g. ["A","B","D"] to tick a multi-select), run in order. Use instead of a "|"-joined names string when the labels themselves contain "|".',
      },
      action: { type: 'string', description: '"auto" (default), "select", "invoke" or "legacy".' },
      observe: { type: 'boolean', description: 'Re-read the window text after acting (default true).' },
      verify: {
        type: 'boolean',
        description: 'Compare the page text before and after each step and report applied/unchanged '
          + '(default true). A step that returned ok but changed nothing is the exact failure that made a '
          + 'multi-select page look answered when it was not, so this is on unless you know better.',
      },
    },
    output: { schema: OUTPUT_SCHEMA, render(_a, v) { return renderValue(v) } },
    async execute(args, exec) {
      const win = args && args.window
      const hwnd = Number(win && win.id)
      const steps = []
      if (Array.isArray(args && args.steps)) {
        for (const step of args.steps) if (typeof step === 'string' && step.trim()) steps.push(step.trim())
      }
      if (typeof (args && args.names) === 'string') {
        for (const step of args.names.split('|')) if (step.trim()) steps.push(step.trim())
      }
      if (!steps.length) throw new Error('names (or steps) is required; use "|" or an array to chain several steps')
      const doVerify = !(args && args.verify === false)
      // A cheap fingerprint of what the page currently says. Not a screenshot: this path
      // exists precisely for windows that cannot be captured.
      const pageFingerprint = () => {
        const nodes = uiaTreeFallback(hwnd, 400)
        if (!Array.isArray(nodes) || nodes.length === 0) return ''
        return summarizeTree(formatUiaFallback(nodes)).slice(0, 4000)
      }
      const acted = []
      for (const step of steps) {
        const before = doVerify ? pageFingerprint() : ''
        const res = uiaActFallback(hwnd, step, args && args.action) || {}
        // Every field must be a defined JSON value: an `undefined` member makes the
        // harness reject the whole result as "not lossless JSON".
        const row = {
          name: step,
          ok: res.ok === true,
          action: typeof res.action === 'string' ? res.action : 'none',
          matched: typeof res.matched === 'string' ? res.matched : '',
        }
        if (res.error) row.error = String(res.error)
        // Let the page settle before judging whether the click did anything.
        await new Promise((resolve) => setTimeout(resolve, 260))
        if (doVerify && row.ok) {
          const after = pageFingerprint()
          const verdict = judgeActStep(before, after, row.ok)
          if (verdict.applied === true) row.applied = true
          if (verdict.unchanged) { row.applied = false; row.unchanged = true; row.hint = verdict.hint }
        }
        acted.push(row)
      }
      // ok now means "issued AND (when verifiable) actually changed the page".
      const value = {
        acted,
        ok: acted.every((a) => a.ok && a.applied !== false),
        unchanged: acted.filter((a) => a.unchanged).map((a) => a.name),
      }
      const wantObserve = args && args.observe !== false
      if (wantObserve) {
        const nodes = uiaTreeFallback(hwnd, 400)
        if (Array.isArray(nodes) && nodes.length) {
          value.accessibility = { tree: formatUiaFallback(nodes), semantic: { dead: false, source: 'uia-dotnet-fallback', nodes: nodes.length } }
          state.lastElements = extractInteractiveElements(formatUiaFallback(nodes))
          state.lastWindow = { app: win.app, id: win.id, title: win.title }
        }
      }
      return { value }
    },
    isConcurrencySafe() { return false },
    presentCall(a) { return { card: 'generic', title: 'computer_use_uia_act', rawInput: a && typeof a === 'object' ? a : {} } },
  })
}

// ---------------------------------------------------------------------------
// Observation/upgrade helpers: screenshot-id externalisation + wedge memory.
// ---------------------------------------------------------------------------

/**
 * Hand out monotonically increasing external screenshot ids so a reused helper id
 * (e.g. "screenshot-0" after the helper's capture session resets) can never be
 * mistaken for "not the latest observation". The mapping external→helper is kept
 * so an input action's screenshotId can be translated back before it reaches the
 * helper (which only knows its own ids).
 */
export function externalizeScreenshotIds(state, value) {
  if (!value || !Array.isArray(value.screenshots)) return
  state.shotSeq = Number.isInteger(state.shotSeq) ? state.shotSeq : 0
  state.shotMap = state.shotMap instanceof Map ? state.shotMap : new Map()
  for (const shot of value.screenshots) {
    if (!shot || typeof shot.id !== 'string') continue
    const helperId = shot.id
    state.shotSeq += 1
    const ext = `shot-${state.shotSeq}`
    state.shotMap.set(ext, helperId)
    shot.id = ext
    shot.helperId = helperId
    // Remember the window's pixel size so scroll_to can default to its centre.
    if (!state.lastShotMeta && shot.width && shot.height) {
      state.lastShotMeta = { width: shot.width, height: shot.height }
    }
  }
}

/** Translate an external screenshotId back to the helper's id, in place. */
export function translateScreenshotId(state, input) {
  if (!input || typeof input.screenshotId !== 'string') return
  const map = state.shotMap
  if (map instanceof Map && map.has(input.screenshotId)) input.screenshotId = map.get(input.screenshotId)
}

/**
 * P1-3: translate the external `shot-N` ids that live *inside* a batched call.
 *
 * `batch_actions` carries its steps in `actions[]` and `fill_form` in `fields[]`;
 * the top-level pass above never sees them, so the helper answered
 * `unknown screenshotId` for a `shot-N` that worked fine as a top-level `click`.
 * Doing it here keeps the translation in one place instead of teaching every
 * tool about external ids.
 */
export function translateNestedActionIds(state, input) {
  if (!input || typeof input !== 'object') return
  const visit = (node) => {
    if (!node || typeof node !== 'object') return
    if (Array.isArray(node)) { node.forEach(visit); return }
    if (typeof node.screenshotId === 'string') translateScreenshotId(state, node)
    if (node.arguments && typeof node.arguments === 'object') visit(node.arguments)
    if (node.args && typeof node.args === 'object') visit(node.args)
    if (Array.isArray(node.actions)) node.actions.forEach(visit)
    if (Array.isArray(node.fields)) node.fields.forEach(visit)
  }
  if (Array.isArray(input.actions)) input.actions.forEach(visit)
  if (Array.isArray(input.fields)) input.fields.forEach(visit)
}

/**
 * Remember that this app's UIA provider is wedged.
 *
 * The 60s cooldown is always recorded. When `state.fallbackLatch` is on (the
 * `fallbackLatchAfterWedge` config, default true), the FIRST wedge also latches
 * this app to the .NET fallback for the rest of the session, so a flaky helper can
 * never keep charging the 15s dump timeout again.
 */
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
export function friendlyActionError(state, input, rawMessage) {
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

//: Tools whose coordinate form needs a cached capture session from the last
//: observation; the first observation of a cold/just-restarted helper can leave
//: that session empty. See {@link recoverCoordinateRefusal}.
const COORDINATE_TOOLS = new Set(['click', 'scroll', 'drag'])

/**
 * Recover a coordinate action refused only because no screenshot session was cached
 * (first observation after helper start) or because the caller's screenshotId was
 * superseded. The refusal means the action never ran, so a single re-observe + retry
 * is safe and turns a two-round-trip model dance into zero. Anything else is left
 * untouched for the caller to see.
 */
export async function recoverCoordinateRefusal(ctx, state, spec, exec, input, result, turnMeta) {
  if (!result || result.ok || !result.error) return result
  if (!COORDINATE_TOOLS.has(spec.name)) return result
  const message = String(result.error)
  const recoverable = /none is cached/i.test(message)
    || /not the latest observation/i.test(message)
    || /unknown screenshotId/i.test(message)
    || /bounds changed before coordinate input/i.test(message)
    || /window bounds changed/i.test(message)
    // P1-4: the monitor cannot tell our own injected click from the human's when the
    // input came from outside the helper (the uia_act click fallback, or a batch step
    // whose synthetic window already expired). The action never ran, so a single
    // re-observe clears the dirty flag and the retry is safe.
    || /user input was detected/i.test(message)
    // P1-5: on a static page the only lease that ever exists is the stale/GDI frame, so
    // treat "no screenshot cached" as recoverable instead of a deadlock.
    || /needs a screenshot from the last observation/i.test(message)
  if (!recoverable) return result
  if (!input || typeof input.window !== 'object' || input.window === null) return result
  try {
    // Cheap un-wedge first: the helper's `clear_user_input` satisfies the "call
    // get_window_state before continuing" contract WITHOUT a capture, which is the only
    // thing that works on windows Windows Graphics Capture can never frame (static page,
    // window bigger than the desktop, covered window). Without it the dirty flag was
    // permanent and every later action answered the same refusal forever.
    try { await ctx.dshComputerUse.call('clear_user_input', {}, exec.signal, turnMeta) } catch { /* older helper */ }
    const observed = await ctx.dshComputerUse.call(
      'get_window_state',
      { window: input.window, include_screenshot: true, include_text: false },
      exec.signal,
      turnMeta,
    ).catch(() => null)
    const value = observed && observed.value !== undefined ? observed.value : observed
    if (value?.accessibility?.tree) {
      state.lastElements = extractInteractiveElements(value.accessibility.tree)
      state.lastWindow = { app: input.window.app, id: input.window.id, title: input.window.title }
    }
    // The refusal means the action never ran, so the retry is safe even when the
    // observation itself could not produce a frame.
    const retryInput = { ...input }
    delete retryInput.screenshotId
    const retried = await ctx.dshComputerUse.call(spec.name, retryInput, exec.signal, turnMeta)
    return retried || result
  } catch {
    return result
  }
}

//: Tools that accept `text` as "the element named X" rather than as a payload.
//: Mirrors exactly the tools whose schema gets the optional `text` property in
//: {@link makeSidecarTool}. Every other tool must keep `text` intact:
//: `type_text` types it, `session_note` stores it.
const TEXT_SELECTOR_TOOLS = new Set(['click', 'set_value', 'perform_secondary_action'])


const JSON_SCHEMA_TYPES = new Set(['object', 'array', 'string', 'number', 'integer', 'boolean', 'null'])
/**
 * 规范 sidecar 返回的 JSON Schema：新版 harness（dsh-tools assertSupportedJsonSchema）只接受
 * 合法单值 type。sidecar 可能返回数组 type 或别名，会被整份工具定义拒绝注册（skip）。
 * 递归修正为合法单值，不改变语义（多类型取首个合法）。
 */
function normalizeSchemaTypes(node, path = "schema") {
  if (node === null || typeof node !== 'object') return
  if (Array.isArray(node)) { node.forEach((x, i) => normalizeSchemaTypes(x, path + '[' + i + ']')); return }
  if (node.type !== undefined) {
    if (Array.isArray(node.type)) {
      const pick = node.type.find(x => JSON_SCHEMA_TYPES.has(x))
      console.error('[dsh-computer-use] type[] @' + path + ': ' + JSON.stringify(node.type) + ' -> ' + String(pick))
      if (pick !== undefined) node.type = pick; else delete node.type
    } else if (typeof node.type === 'string' && !JSON_SCHEMA_TYPES.has(node.type)) {
      const MAP = { any: 'string', json: 'object', dynamic: 'string', auto: 'string' }
      const next = MAP[node.type] ?? "string"
      console.error('[dsh-computer-use] type @' + path + ': "' + node.type + '" -> "' + next + '"')
      node.type = next
    }
  }
  if (node.properties && typeof node.properties === 'object') for (const k of Object.keys(node.properties)) normalizeSchemaTypes(node.properties[k], path + '.properties.' + k)
  if (node.items !== undefined) normalizeSchemaTypes(node.items, path + ".items")
  for (const key of ['anyOf', 'oneOf', 'allOf']) if (Array.isArray(node[key])) node[key].forEach((x, i) => normalizeSchemaTypes(x, path + '.' + key + '[' + i + ']'))
  if (node.additionalProperties && typeof node.additionalProperties === 'object') normalizeSchemaTypes(node.additionalProperties, path + '.additionalProperties')
}

/**
 * P1-4: run a batch one step at a time so each coordinate step can be re-grounded.
 *
 * The helper's own batch runs every step back to back; its human-input monitor then
 * flags the *second* coordinate action as human input ("user input was detected in
 * this window") because the synthetic window covering our injection had already
 * expired. Splitting the batch here also lets every step carry its own translated
 * `shot-N` id (P1-3) and lets the failure name the step that broke.
 *
 * @returns {Promise<{ok: boolean, value?: object, results: object[], failedAt?: number, error?: string}>}
 */
export async function runBatchActions(ctx, state, exec, turnMeta, input) {
  const actions = Array.isArray(input && input.actions) ? input.actions : []
  if (!actions.length) return { ok: false, results: [], error: 'batch_actions needs a non-empty actions[] array (each step: { name, arguments })' }
  const shared = input && typeof input.window === 'object' ? input.window : null
  const results = []
  let lastCoordinate = false
  const observe = async (target) => {
    try {
      await ctx.dshComputerUse.call(
        'get_window_state',
        { window: target, include_screenshot: true, include_text: false },
        exec.signal,
        turnMeta,
      )
    } catch { /* the retry below reports the failure that matters */ }
  }
  for (let i = 0; i < actions.length; i += 1) {
    const action = actions[i]
    const stepName = String((action && action.name) || '')
    if (!stepName) return { ok: false, results, failedAt: i + 1, error: `batch_actions step ${i + 1} has no name` }
    const stepArgs = action && typeof action.arguments === 'object' && action.arguments ? { ...action.arguments } : {}
    if (shared && stepArgs.window === undefined) stepArgs.window = shared
    translateScreenshotId(state, stepArgs)
    translateNestedActionIds(state, stepArgs)
    const coordinate = COORDINATE_TOOLS.has(stepName)
    // Two coordinate actions in a row: renew the lease *before* the second one instead
    // of letting it fail and recovering after a wasted round trip.
    if (lastCoordinate && coordinate) await observe(stepArgs.window || shared)
    const row = { step: i + 1, name: stepName, ok: false }
    let res
    try {
      res = await ctx.dshComputerUse.call(stepName, stepArgs, exec.signal, turnMeta)
    } catch (error) {
      row.error = String(error && error.message ? error.message : error)
      results.push(row)
      return { ok: false, results, failedAt: i + 1, error: `batch_actions step ${i + 1} (${stepName}) threw: ${row.error}` }
    }
    row.ok = res ? res.ok !== false : false
    if (!row.ok && res && res.error) row.error = String(res.error)
    results.push(row)
    if (!row.ok) {
      const recoverable = coordinate && /user input was detected|needs a screenshot|none is cached|not the latest observation|bounds changed/i.test(String(row.error || ''))
      if (recoverable) {
        await observe(stepArgs.window || shared)
        try {
          const retry = await ctx.dshComputerUse.call(stepName, stepArgs, exec.signal, turnMeta)
          row.ok = retry ? retry.ok !== false : false
          row.retried = true
          if (row.ok) delete row.error
          else if (retry && retry.error) row.error = String(retry.error)
        } catch (error) {
          row.error = String(error && error.message ? error.message : error)
        }
      }
      if (!row.ok) {
        return { ok: false, results, failedAt: i + 1, error: `batch_actions step ${i + 1} (${stepName}) failed: ${row.error || 'unknown error'}` }
      }
    }
    lastCoordinate = coordinate
    await new Promise((resolve) => setTimeout(resolve, 60))
  }
  return { ok: true, results, value: { executed: results.length, results } }
}

function makeSidecarTool(ctx, spec, config, state, options) {
  const parameters = spec.parameters && typeof spec.parameters === 'object'
    ? JSON.parse(JSON.stringify(spec.parameters))
    : { type: 'object', properties: {}, additionalProperties: true }
  normalizeSchemaTypes(parameters, 'parameters')
  if (spec.name === 'click') {
    if (parameters.properties && !parameters.properties.text) {
      parameters.properties.text = {
        description: 'Optional element text or name to click (automatically matches element_index from latest get_window_state accessibility tree).',
        type: 'string',
      }
    }
  } else if (spec.name === 'set_value' || spec.name === 'perform_secondary_action') {
    if (parameters.properties && !parameters.properties.text) {
      parameters.properties.text = {
        description: 'Optional element text or name to target (automatically matches element_index from latest get_window_state accessibility tree).',
        type: 'string',
      }
    }
  } else if (spec.name === 'get_window_state') {
    // DSH extension: capture only a window-relative rectangle, cropped locally.
    if (parameters.properties && !parameters.properties.region) {
      parameters.properties.region = {
        type: 'object',
        description: 'Capture only this window-relative rectangle {x,y,width,height} instead of the whole window (saves tokens for long documents).',
        properties: {
          x: { type: 'number', description: 'Left offset (window-relative).' },
          y: { type: 'number', description: 'Top offset (window-relative).' },
          width: { type: 'number', description: 'Crop width.' },
          height: { type: 'number', description: 'Crop height.' },
        },
      }
    }
  }
  const base = spec.description || spec.name
  const description = options?.descriptionPrefix ? `${options.descriptionPrefix}\n\n${base}` : base
  return {
    name: spec.name,
    description,
    parameters,
    // The sidecar owns the official request budget (10 s / 15 s for launch_app)
    // and rejects first; this is only a backstop so a wedged helper can never
    // hold a tool call open forever.
    timeoutMs: 25_000,
    output: {
      schema: OUTPUT_SCHEMA,
      render(_args, value) {
        return renderValue(value)
      },
    },
    async execute(args, exec) {
      // The safety contract runs **first**, before argument validation: a
      // capability that is off has to report the same actionable message no
      // matter what the model passed. It also decides whether this call moves
      // the desktop, which is exactly what the cross-session write lease
      // arbitrates (read operations are never arbitrated).
      const safety = ctx.dshComputerUse?.safety
      const gate = safety ? safety.gate(exec, spec.name) : null
      const input = args && typeof args === 'object' ? { ...args } : {}
      // set_value takes the value in `value`; `text` is only the element selector.
      // Writing the value into `text` reaches the helper as a missing `value` and
      // fails with "value is required", which reads like a schema error rather
      // than "put it in the other field" (tools-improved #23). Checked before the
      // selector pass so the answer names the real mistake, not "no match".
      if (spec.name === 'set_value' && input.value === undefined
          && typeof input.text === 'string' && input.text.trim()
          && input.element_index === undefined) {
        throw new Error(
          `set_value needs its value in "value", not "text" ("${input.text.trim()}" was read as an element name). `
          + 'Pass value:"…" with element_index, or use type_text.',
        )
      }
      // "text as element name" is a convenience of three tools only; for every
      // other tool `text` is the payload. This block used to run unconditionally
      // and then `delete input.text`, so `type_text({window, text})` reached the
      // helper as `{window}` and failed with "text is required" — the payload
      // never survived the selector pass. `session_note` lost its note the same
      // way. Guarding on the tool keeps both intact.
      if (TEXT_SELECTOR_TOOLS.has(spec.name)) {
        const targetText = typeof input.text === 'string' ? input.text.trim() : ''
        if (targetText && input.element_index === undefined && (input.x === undefined || input.y === undefined)) {
          // Audit P1-4 (2026-10-25): the cached indices belong to ONE observed
          // window. Matching text against them for a *different* window silently
          // clicks the wrong control, so the cache is only consulted when the
          // caller's window matches the observation (or names no window at all).
          const cacheForThisWindow = state.lastElements && state.lastElements.length > 0
            && (!input.window || typeof input.window !== 'object' || !state.lastWindow
              || (String(input.window.app || '') === String(state.lastWindow.app || '')
                && (input.window.id === undefined || state.lastWindow.id === undefined
                  || input.window.id === state.lastWindow.id)))
          if (cacheForThisWindow) {
            const matched = state.lastElements.find(e => e.name && e.name.toLowerCase().includes(targetText.toLowerCase()))
              || state.lastElements.find(e => e.raw && e.raw.toLowerCase().includes(targetText.toLowerCase()))
            if (matched) {
              input.element_index = matched.index
            }
          }
          // The text named no element: say so, with the nearest candidates,
          // instead of letting the helper answer `click requires x` (which reads
          // as "you forgot the coordinates" and sends the model chasing the wrong
          // thing — 2026-10-22/23). Only reachable with no index and no x/y.
          if (input.element_index === undefined) {
            const hay = targetText.toLowerCase()
            const near = (state.lastElements || [])
              .filter(e => typeof e.name === 'string' && e.name)
              .map(e => e.name)
              .filter(name => {
                const n = name.toLowerCase()
                // cheap similarity: share a substring of 3+ chars either way
                return n.includes(hay.slice(0, 3)) || hay.includes(n.slice(0, 3))
              })
              .slice(0, 5)
            const hint = near.length
              ? ` Closest: ${near.join(' | ')}`
              : (state.lastElements && state.lastElements.length
                  ? ' Call get_window_state again and pass element_index.'
                  : ' No observation yet: call get_window_state first.')
            throw new Error(
              `${spec.name} found no element matching text "${targetText}" in the current observation.${hint}`,
            )
          }
        }
        delete input.text
      }

      const required = REQUIRED_DOCS[spec.name]
      if (required) {
        const missing = required.filter(doc => !documentWasRead(state.readDocuments, doc))
        if (missing.length > 0) {
          throw new Error(
            `Computer Use cannot run ${spec.name} until you read ${missing.join(', ')} (official documents.json requiredFor gate); ` +
            'it ships under the computer-use-browser skill\'s references/ directory, and the ' +
            'Windows confirmations policy does not satisfy it.',
          )
        }
      }
      const turnMeta = turnMetaFor(ctx.dshComputerUse, exec)
      // Audit P3-3: the try/catch here used to rethrow unchanged — a dead
      // remnant of the removed recordCall hook — so the bare try is gone too.
      // (`startedAt` went with it: the removed hook was its only reader.)
      //
      // The write lease is held for the **whole** mutating region, including the
      // approval retry: the retry is a second desktop mutation, and a lease that
      // was released between the two would let another session take the desktop
      // in the middle of this one. The idle clock restarts when the write
      // *finishes*, not when it was accepted.
      let result
      let captureFailed = false
      let captureDegraded = null
      translateScreenshotId(state, input)
      // P1-3: `batch_actions` carries its sub-actions in `actions[]`, so the top-level
      // translation above never reaches their screenshotId and the helper rejects the
      // external `shot-N` with `unknown screenshotId`.
      translateNestedActionIds(state, input)
      // P1-3/P1-4: batches are driven here, one step at a time, so every coordinate step
      // gets a fresh observation lease and its own translated screenshot id.
      if (spec.name === 'batch_actions') {
        if (gate?.write) safety.beginWrite()
        let outcome
        try {
          outcome = await runBatchActions(ctx, state, exec, turnMeta, input)
        } finally {
          if (gate?.write) safety.finishWrite()
        }
        if (gate?.write) safety.lease.note(gate.sessionId, spec.name)
        if (!outcome.ok) throw friendlyActionError(state, input, outcome.error) || new Error(String(outcome.error))
        return { value: outcome.value, images: [] }
      }
      const wantText = spec.name === 'get_window_state' && Boolean(input && input.include_text)
      const wantShot = spec.name === 'get_window_state'
        ? (input && input.include_screenshot === undefined ? true : Boolean(input && input.include_screenshot))
        : false
      const chromium = looksLikeChromiumHost(input && input.window)
      // Serve accessibility text from the .NET ControlView fallback (richer, fast, no
      // 15s helper dump) when the window is a Chromium/WebView2 host OR when this app is
      // already wedged/latched to the fallback.
      const textViaFallback = wantText && (chromium || isWedged(state, input && input.window && input.window.app))
      // Pure text-only observation of a Chromium host: never touch the helper/capture at
      // all — read the .NET fallback directly. This keeps DRM/hardware-overlay windows
      // (where WGC capture times out) observable and yields a valid observation lease.
      const textOnlySkipHelper = spec.name === 'get_window_state' && wantText && !wantShot && chromium
      const region = spec.name === 'get_window_state' ? normalizeRegion(input && input.region) : null
      const callInput = (() => {
        const c = textViaFallback ? { ...input, include_text: false } : { ...input }
        if (spec.name === 'get_window_state') delete c.region
        return c
      })()
      if (gate?.write) safety.beginWrite()
      const hwndForCapture = Number(input && input.window && input.window.id)
      try {
        if (textOnlySkipHelper) {
          result = { ok: true, value: { accessibility: null }, images: [] }
        } else {
          // A window that already needed a repaint (static page) is nudged BEFORE the
          // helper capture, so it does not pay the 15 s timeout on every observation.
          const preNudge = spec.name === 'get_window_state' && wantShot
            && Number.isInteger(hwndForCapture) && state.nudgeNext instanceof Set && state.nudgeNext.has(hwndForCapture)
          if (preNudge) nudgeWindow(hwndForCapture)
          const attempt = () => ctx.dshComputerUse.call(spec.name, callInput, exec.signal, turnMeta)
          try {
            result = await attempt()
          } catch (thrown) {
            const thrownMsg = thrown && thrown.message ? thrown.message : String(thrown)
            // `covered by another window` is the same class of failure as a timeout: the
            // helper refuses to hand over pixels that belong to a covering window, so the
            // observation would die with no frame. PrintWindow renders the TARGET's own
            // surface (verified: a covered 782x735 window yields a 71 KB PNG), so it is a
            // valid recovery here too.
            const timedOut = /capture timed out|window capture timed out/i.test(thrownMsg)
            const covered = /covered by another window|no trustworthy frame/i.test(thrownMsg)
            if (spec.name === 'get_window_state' && (timedOut || covered)) {
              // P0-1: degrade instead of failing the whole observation:
              // nudge (+ activate when the window was covered) → retry keeps the helper's
              // own frame; then PrintWindow; then the last good frame; then text-only.
              captureFailed = true
              const retry = async () => {
                if (covered && input && input.window) {
                  try { await ctx.dshComputerUse.call('activate_window', { window: input.window }, exec.signal, turnMeta) } catch { /* retry reports it */ }
                }
                return attempt()
              }
              const degraded = wantShot
                ? await captureFallback(state, hwndForCapture, retry)
                : { result: null, path: null, stale: false, geometry: null }
              if (degraded.path === 'wgc-after-nudge' && covered) degraded.path = 'wgc-after-activate'
              captureDegraded = degraded
              if (degraded.result) result = degraded.result
              else result = { ok: true, value: { accessibility: null }, images: [] }
            } else {
              throw friendlyActionError(state, input, thrownMsg) || thrown
            }
          }
          if (result && result.approvalRequest) {
            const refusal = normalizeApprovalRequest(result.approvalRequest)
            // Official helper_transport.js only treats the payload as an approval when
            // "app" is a non-empty string after trimming; otherwise the helper returned
            // an ordinary error and this is not an approval flow (APS-10).
            if (refusal) result = await resolveRefusal(ctx, config, state, spec, exec, input, turnMeta, refusal)
          }
          // First-observation usability: a coordinate click/scroll/drag can be refused
          // because the previous get_window_state cached no capture session (a cold or
          // just-restarted helper), or because the caller passed a stale screenshotId, or
          // because the window bounds changed (RDP resize). All are recoverable without
          // the model re-deriving the plan: re-observe once, drop the stale id, retry.
          result = await recoverCoordinateRefusal(ctx, state, spec, exec, input, result, turnMeta)
        }
      } finally {
        if (gate?.write) safety.finishWrite()
      }
      if (gate?.write) safety.lease.note(gate.sessionId, spec.name)
      // Keep the last good frame for this window: a later live-capture timeout can then
      // serve it flagged stale instead of leaving the model with no pixels at all.
      if (spec.name === 'get_window_state' && Array.isArray(result?.images) && result.images[0]?.data
          && Number.isInteger(hwndForCapture)) {
        rememberFrame(state, hwndForCapture, {
          data: result.images[0].data,
          width: result.images[0].width,
          height: result.images[0].height,
        })
        bumpCaptureStat(state, captureDegraded ? 'frame-recovered' : 'wgc')
      }
      if (!result?.ok && result?.error) {
        const msg = String(result.error)
        throw friendlyActionError(state, input, msg) || new Error(msg)
      }
      result = deriveAudioResult(result, spec.name)
      const value = result?.value ?? result
      // P1-7: keep the window-id whitelist fresh so a reopened app (new HWND) is caught
      // instead of being silently driven through its stale handle. list_windows refreshes
      // the whole set; any other successful call vouches for the window it touched.
      if (spec.name === 'list_windows') {
        const rows = Array.isArray(value) ? value : (value && Array.isArray(value.windows) ? value.windows : [])
        const ids = rows.map((row) => Number(row && row.id)).filter((id) => Number.isInteger(id) && id > 0)
        if (ids.length > 0) state.knownWindowIds = ids
      } else if (Number.isInteger(Number(input && input.window && input.window.id)) && result?.ok !== false) {
        const id = Number(input.window.id)
        state.knownWindowIds = Array.from(new Set([...(state.knownWindowIds || []), id]))
      }
      if (spec.name === 'get_window_state' && value && typeof value === 'object') {
        // Monotonic external screenshot ids (see externalizeScreenshotIds).
        externalizeScreenshotIds(state, value)
        const win = input && typeof input.window === 'object' ? input.window : null
        const app = win ? String(win.app || '') : ''
        const wantText = Boolean(input && input.include_text)
        const ax = value.accessibility && typeof value.accessibility === 'object' ? value.accessibility : null
        const axError = ax && typeof ax.error === 'string' ? ax.error : ''
        if (axError && /wedged|cooldown|no answer within|timeout/i.test(axError)) noteWedge(state, app)
        let tree = ax && typeof ax.tree === 'string' ? ax.tree : ''
        let source = tree ? 'helper' : ''
        // Chromium/WebView2 hosts: serve text from the .NET ControlView fallback —
        // faster than the helper and it reaches content the helper under-reports.
        if (wantText && (looksLikeChromiumHost(win) || isWedged(state, app))) {
          const fb = uiaTreeFallback(Number(win && win.id))
          if (Array.isArray(fb) && fb.length >= 3) {
            tree = formatUiaFallback(fb)
            source = 'uia-dotnet-fallback'
            if (!value.accessibility || typeof value.accessibility !== 'object') value.accessibility = {}
            value.accessibility.tree = tree
            value.accessibility.semantic = { dead: false, source, nodes: fb.length }
            const docUrls = collectDocumentUrls(fb)
            if (docUrls.length) value.accessibility.documentUrls = docUrls
            state.lastElements = []
            state.lastWindow = win ? { app: win.app, id: win.id, title: win.title } : null
          }
        }
        if (tree) {
          if (!(value.accessibility && value.accessibility.semantic)) {
            if (!value.accessibility || typeof value.accessibility !== 'object') value.accessibility = {}
            const classification = classifyAccessibilityTree(tree, win)
            if (classification.dead) {
              const fb2 = uiaTreeFallback(Number(win && win.id))
              if (Array.isArray(fb2) && fb2.length >= 3) {
                value.accessibility.tree = formatUiaFallback(fb2)
                value.accessibility.semantic = { dead: false, source: 'uia-dotnet-fallback', nodes: fb2.length, helperNodes: classification.total }
                state.lastElements = []
                state.lastWindow = win ? { app: win.app, id: win.id, title: win.title } : null
              } else {
                const cdp = await cachedCdpProbe(state, win)
                value.accessibility.tree = deadTreeSignal(classification, cdp)
                value.accessibility.semantic = { dead: true, totalNodes: classification.total, meaningfulNodes: classification.meaningful, fallbackTried: true, cdpAttachable: Boolean(cdp && cdp.canAttach) }
                state.lastElements = []
                state.lastWindow = win ? { app: win.app, id: win.id, title: win.title } : null
              }
            } else {
              const compressed = compressAccessibilityTree(tree)
              value.accessibility.tree = compressed
              value.accessibility.semantic = { dead: false, totalNodes: classification.total, meaningfulNodes: classification.meaningful, source }
              state.lastElements = extractInteractiveElements(compressed)
              state.lastWindow = win ? { app: win.app, id: win.id, title: win.title } : null
            }
          }
        } else if (wantText && (axError || isWedged(state, app))) {
          if (!value.accessibility || typeof value.accessibility !== 'object') value.accessibility = {}
          value.accessibility.guidance = 'UIA provider is wedged/unavailable for this window. The SCREENSHOT IS STILL VALID — act by x/y coordinates; accessibility text is unavailable right now.'
          value.accessibility.semantic = { dead: false, source: 'none-wedged' }
        }
        if (captureFailed) {
          if (!value.accessibility || typeof value.accessibility !== 'object') value.accessibility = {}
          const guidance = captureGuidance(captureDegraded)
          value.accessibility.guidance = guidance
          const recovered = Boolean(captureDegraded && captureDegraded.result)
          value.capture = {
            failed: true,
            path: (captureDegraded && captureDegraded.path) || null,
            stale: Boolean(captureDegraded && captureDegraded.stale),
            recovered,
            ...(captureDegraded && captureDegraded.ageMs ? { ageMs: captureDegraded.ageMs } : {}),
            ...(captureDegraded && captureDegraded.geometry && captureDegraded.geometry.oversize ? { oversizeWindow: true } : {}),
            guidance,
          }
          // Only a fully unrecovered observation keeps the historical flag, which other
          // layers (and the skill doc) treat as "no pixels at all".
          if (!recovered) value.captureFailed = true
        }
      }
      // Region crop (token saving): crop each captured frame to the requested rectangle.
      if (spec.name === 'get_window_state' && region && Array.isArray(result?.images)) {
        const cropped = []
        for (const im of result.images) {
          if (!im || !im.data) { cropped.push(im); continue }
          const out = cropImageBase64(im.data, im.mimeType, region, { x: im.originX, y: im.originY }, { width: im.width, height: im.height })
          cropped.push(out ? { ...im, data: out, mimeType: 'image/png' } : im)
        }
        result.images = cropped
        if (Array.isArray(value?.screenshots)) {
          for (const s of value.screenshots) {
            s.width = Math.min(s.width || region.width, region.width)
            s.height = Math.min(s.height || region.height, region.height)
            s.originX = (s.originX || 0) + region.x
            s.originY = (s.originY || 0) + region.y
          }
        }
        if (value && typeof value === 'object') value.region = region
      }
      const refs = await admitImages(ctx, result?.images || [])
      return { value, images: refs }
    },
    isConcurrencySafe() {
      return false
    },
    presentCall(args) {
      return {
        card: 'generic',
        title: spec.name,
        rawInput: args && typeof args === 'object' ? args : {},
      }
    },
  }
}

/**
 * APS-09: official `Audio.d.ts` is `{ filepath, bytes, data_url }` ("Raw 24 kHz
 * stereo WAV bytes") and the official Windows client derives `bytes`/`data_url` by
 * re-reading the file the helper wrote (`computer_use_client_base.js`,
 * `stop_audio_recording`):
 *
 *   const audio = await request('stop_audio_recording', {})
 *   if (typeof audio !== 'object' || audio == null) throw new Error('codex-computer-use.exe did not return computer audio')
 *   const filepath = Reflect.get(audio, 'filepath')
 *   if (typeof filepath !== 'string' || filepath.length === 0) throw new Error('codex-computer-use.exe did not return a computer audio filepath')
 *   const bytes = await from_filepath(filepath)
 *   return { filepath, bytes, data_url: to_data_url(bytes, 'audio/wav') }
 *
 * DSH's helper returns `filepath` + `data_url` but no `bytes`, so the plugin closes
 * the gap here. `bytes` is published as the byte COUNT: the official in-process
 * value is a `Uint8Array`, which a model-facing JSON tool result cannot carry
 * faithfully; the payload itself travels in the official `data_url`.
 *
 * @param {object} result the sidecar call result ({ ok, value, images })
 * @param {string} method
 * @returns {object}
 * @throws with the official error strings (the read failure rides along as `cause`)
 */
export function deriveAudioResult(result, method) {
  if (method !== 'stop_audio_recording') return result
  if (result === null || typeof result !== 'object' || Array.isArray(result)) return result
  const audio = result.value !== undefined ? result.value : result
  if (audio === null || typeof audio !== 'object' || Array.isArray(audio)) {
    throw new Error('codex-computer-use.exe did not return computer audio')
  }
  const filepath = typeof audio.filepath === 'string' ? audio.filepath : ''
  if (filepath === '') throw new Error('codex-computer-use.exe did not return a computer audio filepath')
  let bytes
  try {
    bytes = fs.readFileSync(filepath)
  } catch (error) {
    // The official client does not catch this; the closest official string is the
    // filepath one, and the original error stays visible as `cause` (never swallowed).
    throw new Error('codex-computer-use.exe did not return a computer audio filepath', { cause: error })
  }
  const derived = {
    ...audio,
    filepath,
    bytes: bytes.length,
    data_url: 'data:audio/wav;base64,' + bytes.toString('base64'),
  }
  return result.value !== undefined ? { ...result, value: derived } : derived
}

/**
 * Official `helper_transport.js` approval-request parser (APS-10).
 *
 * The official shape is: an approval exists only when `app` is a **non-empty
 * string after trimming**; `displayName` is the trimmed `displayName` or else the
 * trimmed `app` (`helper_transport.js`: `const s = (typeof n === 'string' &&
 * n.trim() !== '') ? n.trim() : i`); `riskLevel` is the two-valued boolean
 * mapping (`'high'`/`'low'`, anything else dropped); and
 * `allowPersistentApproval` defaults to `true`.
 *
 * @param {unknown} raw
 * @returns {{ app: string, displayName: string, riskLevel?: 'low'|'high', allowPersistentApproval: boolean }|null}
 */
export function normalizeApprovalRequest(raw) {
  if (raw === null || raw === undefined || typeof raw !== 'object' || Array.isArray(raw)) return null
  const value = /** @type {Record<string, unknown>} */ (raw)
  const app = typeof value.app === 'string' ? value.app.trim() : ''
  if (app === '') return null
  const displayName = typeof value.displayName === 'string' && value.displayName.trim() !== ''
    ? value.displayName.trim()
    : app
  const risk = value.riskLevel === 'high' || value.riskLevel === 'low' ? value.riskLevel : undefined
  return {
    app,
    displayName,
    allowPersistentApproval: value.allowPersistentApproval !== false,
    ...(risk !== undefined ? { riskLevel: risk } : {}),
  }
}

/**
 * The session's permission facts for the app-approval path.
 *
 * The harness derives two independent knobs from the user's permission preset and
 * writes both into the session log: `sandbox/mode` and `approval/policy`. The shipped
 * base bundle maps full access to `danger-full-access` + `never`, and `never` means
 * "never prompt anyone": ApprovalService.request() resolves "rejected" deterministically
 * *before* any answerer sees the ask (user-approval/src/index.ts decide()). Asking
 * anyway therefore does not merely skip a click, it makes launch_app, activate_window
 * and audio recording unusable for the whole session -- which is exactly the bug this
 * reads the log for. Full access is the user saying "do not ask me", so the app gate is
 * granted instead of asked, and the grant is recorded for the audit trail.
 *
 * @param {object} exec tool execution context; `agent.session` carries the log
 * @param {object} [approval] the approval service, for its configured default policy
 * @returns {{ sandbox: string|undefined, policy: string, fullAccess: boolean, neverAsk: boolean }}
 */
export function approvalFacts(exec, approval) {
  const session = exec && exec.agent ? exec.agent.session : undefined
  let sandbox
  let policy
  if (session && typeof session.seq === 'number' && typeof session.eventAt === 'function') {
    for (let seq = session.seq - 1; seq >= 0 && (sandbox === undefined || policy === undefined); seq -= 1) {
      let event
      try {
        event = session.eventAt(seq)
      } catch {
        break
      }
      if (!event) continue
      if (sandbox === undefined && event.type === 'sandbox/mode') sandbox = event.data && event.data.mode
      if (policy === undefined && event.type === 'approval/policy') policy = event.data && event.data.policy
    }
  }
  const configured = approval && approval.config && typeof approval.config.policy === 'string'
    ? approval.config.policy
    : undefined
  const effective = typeof policy === 'string' && policy !== '' ? policy : (configured || 'ask')
  return {
    sandbox,
    policy: effective,
    fullAccess: sandbox === 'danger-full-access',
    neverAsk: effective === 'never',
  }
}

/**
 * Decide a helper refusal. Official transport elicits **after** the refusal and
 * retries with `x-oai-cua-approved-app`; the plugin must never synthesise that
 * header on its own (APS-01). A session the user put into full access (or one whose
 * approval policy never prompts) has already answered the question, so its refusal is
 * granted without an ask; an explicit `deny` still fails closed.
 *
 * @returns {Promise<object>} the (possibly retried) helper result
 */
async function resolveRefusal(ctx, config, state, spec, exec, input, turnMeta, refusal) {
  const approval = refusal
  const app = approval.app || String(exec.arguments?.app || '')
  const displayName = approval.displayName || app || spec.name
  const isAudio = spec.name === 'start_audio_recording' || spec.name === 'stop_audio_recording'
  const mode = approvalModeFor(config, spec.name)
  if (mode === 'deny') throw new Error(notApprovedMessage(displayName))
  if (mode === 'allow') {
    state.recordedApprovals.push({ toolName: spec.name, app, mode: 'allow' })
    return retryWithApproval(ctx, spec, exec, input, turnMeta, app)
  }
  const service = ctx.get('approval')
  const facts = approvalFacts(exec, service)
  if (facts.fullAccess || facts.neverAsk) {
    // The user already answered the question by choosing this permission preset, and a
    // 'never' policy cannot be answered at all (the service auto-rejects before any
    // answerer). Grant the app for this call and keep the audit trail honest.
    state.recordedApprovals.push({
      toolName: spec.name,
      app,
      mode: 'allow',
      outcome: facts.fullAccess ? 'full-access' : 'approval-policy-never',
    })
    return retryWithApproval(ctx, spec, exec, input, turnMeta, app)
  }
  if (service === undefined || exec.agent === undefined) {
    // No answerer: fail closed exactly like the official unavailable path.
    throw new Error(notApprovedMessage(displayName))
  }
  const allowPersistent = approval.allowPersistentApproval !== false
  const canPersist = allowPersistent && !isAudio
  const request = {
    agent: exec.agent,
    toolName: spec.name,
    callId: exec.callId,
    reason: isAudio
      ? 'Allow Computer Use to record computer audio?'
      : `Allow Computer Use to use ${displayName}?`,
    signal: exec.signal,
  }
  // Official elicitation meta (helper_transport oHT:149-176 / APP-3). DSH's
  // ApprovalRequestEvent has no slot for these, so they ride as extra
  // properties on the same object the answerer receives.
  Object.assign(request, {
    codex_approval_kind: 'mcp_tool_call',
    connector_id: 'computer-use',
    connector_name: 'Computer Use',
    persist: canPersist ? ['session', 'always'] : ['session'],
    riskLevel: approval.riskLevel || 'low',
    tool_params: { app },
    tool_params_display: [{ name: 'app', display_name: 'App', value: displayName }],
    ...(isAudio
      ? { codex_request_type: 'approval_request', tool_call_id: exec.callId, tool_name: 'start_audio_recording' }
      : {}),
  })
  const outcome = await service.request(request)
  if (outcome !== 'allowed-once') throw new Error(notApprovedMessage(displayName))
  state.recordedApprovals.push({ toolName: spec.name, app, mode: 'prompt', outcome: String(outcome) })
  return retryWithApproval(ctx, spec, exec, input, turnMeta, app)
}

function approvalModeFor(config, toolName) {
  const tools = config.approvalTools || {}
  if (Object.prototype.hasOwnProperty.call(tools, toolName)) return tools[toolName]
  return config.approvalDefault || 'prompt'
}

function retryWithApproval(ctx, spec, exec, input, turnMeta, app) {
  return ctx.dshComputerUse.call(spec.name, input, exec.signal, { ...turnMeta, 'x-oai-cua-approved-app': app })
}

/** Official transport wording for a declined approval (oHT:177-189). */
function notApprovedMessage(subject) {
  return `Computer Use was not approved to use ${subject}`
}

/**
 * Extract interactive elements from an accessibility tree for compact inspection.
 * @param {string} treeText
 * @returns {Array<{ index: number, role: string, name: string, raw: string }>}
 */
export function extractInteractiveElements(treeText) {
  if (!treeText || typeof treeText !== 'string') return []
  const lines = treeText.split('\n')
  const elements = []
  const regex = /^\t*(\d+)\s+([^\s]+)(?:\s+(.+))?$/
  for (const line of lines) {
    const match = regex.exec(line)
    if (!match) continue
    const index = parseInt(match[1], 10)
    const role = match[2]
    const rest = match[3] || ''
    const isInteractive = /按钮|选项卡|选项卡项目|菜单项|链接|复选框|单选按钮|编辑|组合框|列表项目|button|tab|tabitem|menuitem|link|checkbox|radiobutton|edit|combobox|listitem/i.test(role)
    if (isInteractive) {
      elements.push({ index, role, name: rest.trim(), raw: line.trim() })
    }
  }
  return elements
}

/**
 * Detect whether the observed window is hosting a Web Document (browser page)
 * and extract routing facts (URL, browser type) to guide the agent toward
 * 10x faster CDP/DOM automation instead of blind coordinate clicks.
 *
 * @param {object} body
 * @returns {{ isWeb: boolean, browser: string, url: string | null } | null}
 */
export function detectWebDocument(body) {
  if (!body || typeof body !== 'object') return null
  const app = String(body.window?.app || '')
  const appLower = app.toLowerCase()
  const isBrowser = /chrome|msedge|edge|brave|firefox|opera|vivaldi|chromium/i.test(appLower)
  const tree = body.accessibility?.tree || ''
  const focused = body.accessibility?.focused_element || ''
  const docText = body.accessibility?.document_text || ''

  // Match URL from focused element, accessibility tree, or document_text
  let url = null
  const urlRegex = /Value:\s*(https?:\/\/[^\s\n\r"'>]+)/i
  const match = urlRegex.exec(focused) || urlRegex.exec(tree)
  if (match) {
    url = match[1]
  } else {
    const textUrlMatch = /(https?:\/\/[^\s\n\r"'>]+)/i.exec(docText)
    if (textUrlMatch) {
      url = textUrlMatch[1]
    }
  }

  const hasWebArea = /RootWebArea|Chrome_RenderWidgetHostHWND|\b文档\b|\bdocument\b/i.test(tree)
  if ((isBrowser && (url || hasWebArea)) || (url && hasWebArea)) {
    const browserName = app.replace(/^process:.*\\/i, '').replace(/\.exe$/i, '')
    return {
      isWeb: true,
      browser: browserName || 'browser',
      url,
    }
  }
  return null
}

/**
 * Format WindowState body with an interactive element summary and trimmed document_text
 * to prevent context explosion and DSH middle-pruning while preserving JSON structure.
 * @param {object} body
 * @returns {string}
 */
export function formatWindowStateBody(body) {
  if (!body || typeof body !== 'object') return stringifyCompact(body)
  const ax = body.accessibility
  if (!ax || typeof ax !== 'object') return stringifyCompact(body)

  let header = ''
  const webDoc = detectWebDocument(body)
  if (webDoc) {
    const urlDisplay = webDoc.url ? ` (${webDoc.url})` : ''
    header = [
      `=== Web Document Detected: Smart Router Advisory ===`,
      `Target window is displaying a Web Document in ${webDoc.browser}${urlDisplay}.`,
      `• Fast Path (Recommended): For web forms, buttons, links, and data reading, prefer CDP / DOM automation (load skill "computer-use-browser" for tab_* tools or use "cdp-browser-use"). DOM scripts execute in ~0.5s without pixel coordinates, screen capture latency, or IME composition issues.`,
      `• Native Path: If staying on the desktop surface, use "fill_form" for atomic multi-field input, or use element_index from the summary below.`,
      `====================================================\n\n`,
    ].join('\n')
  }

  let summary = ''
  if (typeof ax.tree === 'string' && ax.tree.length > 0) {
    const elems = extractInteractiveElements(ax.tree)
    if (elems.length > 0) {
      const summaryLines = [
        `=== Interactive Elements (${elems.length} found; use element_index with click/set_value) ===`,
      ]
      for (const el of elems.slice(0, 60)) {
        summaryLines.push(`[${el.index}] ${el.role} "${el.name}"`)
      }
      if (elems.length > 60) {
        summaryLines.push(`... [${elems.length - 60} more interactive elements in tree]`)
      }
      summary = summaryLines.join('\n') + '\n\n'
    }
  }

  let safeBody = body
  const axOverrides = {}
  if (typeof ax.document_text === 'string' && ax.document_text.length > 600) {
    const total = ax.document_text.length
    axOverrides.document_text =
      ax.document_text.slice(0, 250) +
      `\n... [document_text trimmed: ${total} total characters to prevent middle pruning; see tree for structure] ...\n` +
      ax.document_text.slice(-150)
  }
  // Audit P2-11 (2026-10-25): the raw tree was shipped whole. Deep windows
  // (IDEs, browsers) push past 100k characters, and the interactive summary
  // above already carries what element_index targeting needs, so the raw tree
  // gets a middle-trim budget of its own instead of a free ride.
  if (typeof ax.tree === 'string' && ax.tree.length > 24_000) {
    const total = ax.tree.length
    axOverrides.tree =
      ax.tree.slice(0, 20_000) +
      `\n... [tree trimmed: ${total} total characters; ${total - 22_000} middle characters dropped. Re-run get_window_state with a narrower scope if the element you need is gone] ...\n` +
      ax.tree.slice(-2_000)
  }
  if (Object.keys(axOverrides).length > 0) {
    safeBody = { ...body, accessibility: { ...ax, ...axOverrides } }
  }

  return header + summary + stringifyCompact(safeBody)
}

function renderValue(value) {
  const record = value && typeof value === 'object' ? value : { value }
  const body = record.value === undefined ? record : record.value
  const blocks = []
  if (body !== null && body !== undefined) {
    blocks.push({ type: 'text', text: formatWindowStateBody(body) })
  }
  for (const attachment of Array.isArray(record.images) ? record.images : []) {
    if (attachment && attachment.attachmentId) {
      blocks.push({ type: 'image', attachment })
    }
  }
  return blocks
}

function stringifyCompact(value) {
  try {
    // Audit P2-11: pretty-printing doubled the payload size of every
    // get_window_state result for zero reader benefit — the model consumes
    // JSON, it does not need indentation.
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

async function admitImages(ctx, images) {
  if (images.length === 0) return []
  const attachments = ctx.get('attachments')
  if (attachments === undefined) {
    throw new Error('screenshot captured but attachments service is unavailable; cannot admit vision ImageBlocks')
  }
  const decoded = []
  for (const image of images) {
    if (!image?.data) continue
    const buffer = Buffer.from(image.data, 'base64')
    decoded.push({
      data: new Uint8Array(buffer),
      mediaType: image.mimeType === 'image/jpeg' || image.mimeType === 'image/webp' || image.mimeType === 'image/gif'
        ? image.mimeType
        : 'image/png',
      name: image.name || 'screenshot.png',
    })
  }
  if (decoded.length === 0) return []
  if (typeof attachments.saveImages === 'function') {
    return [...await attachments.saveImages(decoded)]
  }
  if (typeof attachments.saveImage === 'function') {
    const refs = []
    for (const item of decoded) refs.push(await attachments.saveImage(item))
    return refs
  }
  return []
}

