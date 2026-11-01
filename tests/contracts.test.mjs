/**
 * Domain gates for the DSH Computer Use plugin plane.
 *
 * These are static or protocol-level: they need no desktop and no mouse. Run
 * from the harness checkout so the plugin peer dependencies resolve:
 *
 *   cd <harness-checkout>
 *   node --import tsx/esm --test tests/contracts.test.mjs
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import ComputerUseService, { Config as HostConfig, TURN_LIFECYCLE_HOOKS, SERVICE_CAPABILITIES, captureLimits } from '../src/index.js'
import {
  Config as ToolConfig,
  apply as applyTools,
  REQUIRED_DOCS,
  normalizeApprovalRequest,
  normalizeDocPath,
  documentWasRead,
  deriveAudioResult,
  computerUseSectionOrder,
  approvalFacts,
} from '../src/tool.js'
import {
  computerUsePrompt,
  promptAssetStatus,
  ALWAYS_ON_FILES,
  REQUIRED_PROMPT_FILES,
  documentationSummary,
  browserReferenceStatus,
  BROWSER_REFERENCE_FILES,
} from '../src/prompt.js'
import { sanitizedEnvironment, waitSpawn, BASE_ENV_ALLOWLIST, Sidecar } from '../src/sidecar.js'

// The bundle root IS the repository root (official layout: `package.json` +
// `cordis.patch.yml` + entry modules at the package root, with the Rust helper and the
// Python engine beside them), so the package root and the repo root are the same directory.
const pluginRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repoRoot = pluginRoot
const source = name => fs.readFileSync(path.join(pluginRoot, 'src', name), 'utf8')

const DESKTOP_13 = [
  'list_windows', 'get_window', 'list_apps', 'launch_app', 'get_window_state', 'click',
  'press_key', 'type_text', 'scroll', 'set_value', 'drag', 'perform_secondary_action', 'activate_window',
].map(name => ({ name, description: name, parameters: { type: 'object', properties: {} } }))

function makeHarness(options = {}) {
  const handlers = new Map()
  const registered = new Map()
  const sections = []
  const calls = []
  const dshComputerUse = {
    config: {},
    approved: new Set(),
    lastConversationId: null,
    tools: async surface => {
      if (surface === 'browser') return { tools: options.browserTools || [] }
      if (surface === 'computer') return { tools: options.desktopTools || DESKTOP_13 }
      const extras = [
        { name: 'batch_actions', description: 'batch', parameters: {} },
        { name: 'scroll_element', description: 'scroll element', parameters: {} },
        { name: 'fill_form', description: 'fill form', parameters: {} },
        { name: 'diagnostic_state', description: 'diag', parameters: {} },
        { name: 'end_turn', description: 'end', parameters: {} },
      ]
      return { tools: DESKTOP_13.concat(extras) }
    },
    call: async (name, args, signal, meta) => {
      calls.push({ name, args, meta })
      if (options.onCall) return options.onCall(name, args, meta, calls.length)
      return { ok: true, value: { called: name } }
    },
    health: async () => ({ ok: true, codexRequired: false, backend: 'fake', surface: 'computer' }),
    releaseOverlay: async () => ({ ok: true }),
    shutdownSidecar: async () => {},
  }
  const ctx = {
    dshComputerUse,
    on(event, fn) {
      const list = handlers.get(event) || []
      list.push(fn)
      handlers.set(event, list)
      return () => {}
    },
    effect(fn) { fn() },
    get(name) {
      if (name === 'approval') return options.approval
      if (name === 'attachments') return options.attachments
      return undefined
    },
    tools: { register(tool) { registered.set(tool.name, tool) } },
    systemPrompt: { section(section) { sections.push(section) } },
  }
  return {
    ctx,
    dshComputerUse,
    registered,
    sections,
    calls,
    emitResult(exec, result) {
      for (const fn of handlers.get('tools/result') || []) fn(exec, result)
    },
  }
}

test('C1 always-on prompt is the header only and within budget', () => {
  const text = computerUsePrompt()
  const header = fs.readFileSync(path.join(repoRoot, 'helper-rs', 'assets', 'prompts', 'dsh-header.md'), 'utf8').trim()
  assert.equal(text, header, 'always-on text must be exactly dsh-header.md')
  assert.ok(text.length <= 12288, 'always-on chars=' + text.length)
  assert.deepEqual(ALWAYS_ON_FILES, ['dsh-header.md'])
})

test('C7 the always-on contract uses the centrally owned computer-use slot when it exists', () => {
  // 0.1.6 added SECTION_ORDERS.TOOL_COMPUTER_USE next to the other tool contracts.
  assert.equal(computerUseSectionOrder({ getSectionOrder: () => 3000 }), 3000)
  // Older releases have no such entry: the lookup returns undefined and we keep 48.
  assert.equal(computerUseSectionOrder({ getSectionOrder: () => undefined }), 48)
  assert.equal(computerUseSectionOrder({}), 48)
  assert.equal(computerUseSectionOrder(undefined), 48)
})

test('C6 a missing prompt asset fails loudly and is reported by promptAssetStatus', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cu-assets-'))
  try {
    assert.throws(() => computerUsePrompt({ dir }), /prompt assets unavailable/)
    const status = promptAssetStatus(dir)
    assert.equal(status.ok, false)
    for (const name of REQUIRED_PROMPT_FILES) assert.ok(status.missing.includes(name), 'missing ' + name)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('C3 sidecar never writes the JSON-RPC envelope', () => {
  assert.equal(/jsonrpc\s*:/.test(source('sidecar.js')), false, 'no jsonrpc key may be written')
})

test('C4 helper environment is a whitelist, not the parent environment', () => {
  assert.equal(/\.\.\.process\.env/.test(source('sidecar.js')), false, 'no process.env spread')
  const sentinel = 'SECRET_TOKEN_SENTINEL_VALUE'
  process.env.SECRET_TOKEN_SENTINEL = sentinel
  process.env.DSH_HOME = process.env.DSH_HOME || 'C:\\dsh-home'
  try {
    const { env, injected, excluded } = sanitizedEnvironment({ EXTRA_FLAG: '1' }, [])
    assert.equal(env.SECRET_TOKEN_SENTINEL, undefined, 'sentinel must not reach the helper')
    assert.ok(excluded.includes('SECRET_TOKEN_SENTINEL'))
    assert.equal(env.EXTRA_FLAG, '1')
    assert.ok(injected.includes('PATH') || injected.includes('Path'), 'PATH should be forwarded')
    assert.ok(injected.includes('DSH_HOME'))
    assert.ok(BASE_ENV_ALLOWLIST.includes('SystemRoot'))
  } finally {
    delete process.env.SECRET_TOKEN_SENTINEL
  }
})

test('C5 a helper that never becomes ready rejects on a startup budget', async () => {
  const fake = {
    pid: undefined,
    exitCode: null,
    once() {},
    off() {},
    kill() { this.killed = true },
    killed: false,
  }
  await assert.rejects(() => waitSpawn(fake, 30), /startup timeout/)
  assert.equal(fake.killed, true, 'a helper that never becomes ready must be killed')
})

test('TC-01 desktop surface is exactly the official thirteen plus the configured extras', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const names = [...harness.registered.keys()]
  for (const spec of DESKTOP_13) assert.ok(names.includes(spec.name), 'missing ' + spec.name)
  assert.ok(names.includes('batch_actions'), 'configured harness extra missing')
  assert.equal(names.includes('diagnostic_state'), false, 'diagnostic tools are opt-in')
  assert.equal(names.includes('end_turn'), false, 'end_turn is not model-facing')
  assert.equal(names.includes('click_element'), false)
})

test('APS-01 a rejected refusal goes nowhere and is never self-approved', async () => {
  const harness = makeHarness({
    approval: { request: async () => 'rejected' },
    onCall: () => ({ ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'evil.exe', displayName: 'Evil', riskLevel: 'high', allowPersistentApproval: true } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('click')
  await assert.rejects(
    () => tool.execute({ window: { app: 'evil.exe', id: 1 }, x: 1, y: 1 }, { agent: { id: 's' }, arguments: { app: 'evil.exe' } }),
    /not approved to use Evil/,
  )
  const retries = harness.calls.filter(c => c.meta && c.meta['x-oai-cua-approved-app'])
  assert.equal(retries.length, 0, 'a rejected approval must not be retried with the approved header')
})

test('APS-01 an approved refusal retries once with the official header and elicitation meta', async () => {
  let seen
  const harness = makeHarness({
    approval: { request: async request => { seen = request; return 'allowed-once' } },
    onCall: (name, args, meta, count) => (count === 1
      ? { ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'notepad.exe', displayName: 'Notepad', riskLevel: 'low', allowPersistentApproval: true } }
      : { ok: true, value: { done: true } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('launch_app')
  const result = await tool.execute({ app: 'notepad.exe' }, { agent: { id: 's' }, arguments: { app: 'notepad.exe' } })
  assert.equal(result.value.done, true)
  assert.equal(seen.connector_id, 'computer-use')
  assert.equal(seen.connector_name, 'Computer Use')
  assert.deepEqual(seen.persist, ['session', 'always'])
  assert.equal(seen.riskLevel, 'low')
  assert.deepEqual(seen.tool_params, { app: 'notepad.exe' })
  assert.equal(seen.codex_approval_kind, 'mcp_tool_call')
  assert.match(seen.reason, /Allow Computer Use to use Notepad/)
  const retries = harness.calls.filter(c => c.meta && c.meta['x-oai-cua-approved-app'] === 'notepad.exe')
  assert.equal(retries.length, 1)
})

test('C8 approvalFacts folds the session permission log', () => {
  const session = events => ({ seq: events.length, eventAt: seq => events[seq] })
  const full = session([
    { type: 'sandbox/mode', data: { mode: 'danger-full-access' } },
    { type: 'approval/policy', data: { policy: 'never' } },
  ])
  assert.deepEqual(approvalFacts({ agent: { session: full } }), {
    sandbox: 'danger-full-access',
    policy: 'never',
    fullAccess: true,
    neverAsk: true,
  })
  const interactive = session([
    { type: 'sandbox/mode', data: { mode: 'workspace-write' } },
    { type: 'approval/policy', data: { policy: 'ask' } },
  ])
  assert.deepEqual(approvalFacts({ agent: { session: interactive } }), {
    sandbox: 'workspace-write',
    policy: 'ask',
    fullAccess: false,
    neverAsk: false,
  })
  // Only the LAST value of each knob counts (the log is a fold, not a set).
  const flipped = session([
    { type: 'approval/policy', data: { policy: 'never' } },
    { type: 'approval/policy', data: { policy: 'ask' } },
  ])
  assert.equal(approvalFacts({ agent: { session: flipped } }).neverAsk, false)
  // Without a session log the approval service's configured default decides.
  assert.equal(approvalFacts({ agent: { id: 's' } }, { config: { policy: 'never' } }).neverAsk, true)
  assert.equal(approvalFacts(undefined, undefined).policy, 'ask', 'the fail-safe default stays interactive')
})

test('APS-11 a full-access session grants the app refusal without asking', async () => {
  let asked = 0
  const session = {
    seq: 2,
    eventAt: seq => [
      { type: 'sandbox/mode', data: { mode: 'danger-full-access' } },
      { type: 'approval/policy', data: { policy: 'never' } },
    ][seq],
  }
  const harness = makeHarness({
    approval: { request: async () => { asked += 1; return 'allowed-once' } },
    onCall: (name, args, meta, count) => (count === 1
      ? { ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'notepad.exe', displayName: 'Notepad', allowPersistentApproval: true } }
      : { ok: true, value: { done: true } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const exec = { agent: { id: 's', session }, arguments: { app: 'notepad.exe' } }
  const result = await harness.registered.get('launch_app').execute({ app: 'notepad.exe' }, exec)
  assert.equal(result.value.done, true, 'the session-level grant admits the retry')
  assert.equal(asked, 0, 'full access must not ask the user')
  assert.equal(
    harness.calls.filter(c => c.meta && c.meta['x-oai-cua-approved-app'] === 'notepad.exe').length,
    1,
    'the retry still carries the official approved-app header',
  )
  const health = await harness.registered.get('computer_use_health').execute({}, exec)
  assert.equal(health.approval.session.fullAccess, true)
  assert.deepEqual(health.approval.recorded[0], {
    toolName: 'launch_app',
    app: 'notepad.exe',
    mode: 'allow',
    outcome: 'full-access',
  })
})

test('APS-11 a never-prompt policy grants the refusal, while an explicit deny still fails closed', async () => {
  let asked = 0
  const session = {
    seq: 1,
    eventAt: seq => [{ type: 'approval/policy', data: { policy: 'never' } }][seq],
  }
  const harness = makeHarness({
    approval: { request: async () => { asked += 1; return 'allowed-once' } },
    onCall: (name, args, meta, count) => (count === 1
      ? { ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'notepad.exe', displayName: 'Notepad' } }
      : { ok: true, value: { done: true } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const exec = { agent: { id: 's', session }, arguments: { app: 'notepad.exe' } }
  const result = await harness.registered.get('launch_app').execute({ app: 'notepad.exe' }, exec)
  assert.equal(result.value.done, true)
  assert.equal(asked, 0, 'a never-prompt session cannot be asked at all')
  const health = await harness.registered.get('computer_use_health').execute({}, exec)
  assert.equal(health.approval.recorded[0].outcome, 'approval-policy-never')

  // The explicit per-tool deny is configuration, not a session default: it still fails closed.
  const denied = makeHarness({
    approval: { request: async () => 'allowed-once' },
    onCall: () => ({ ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'notepad.exe', displayName: 'Notepad' } }),
  })
  await applyTools(denied.ctx, ToolConfig({ approvalTools: { launch_app: 'deny' } }))
  await assert.rejects(
    () => denied.registered.get('launch_app').execute({ app: 'notepad.exe' }, exec),
    /not approved to use Notepad/,
  )
})

test('APS-11 an interactive session still asks exactly once', async () => {
  let asked = 0
  const session = {
    seq: 2,
    eventAt: seq => [
      { type: 'sandbox/mode', data: { mode: 'workspace-write' } },
      { type: 'approval/policy', data: { policy: 'ask' } },
    ][seq],
  }
  const harness = makeHarness({
    approval: { request: async () => { asked += 1; return 'allowed-once' } },
    onCall: (name, args, meta, count) => (count === 1
      ? { ok: false, error: 'AppApprovalRequired', approvalRequest: { app: 'notepad.exe', displayName: 'Notepad' } }
      : { ok: true, value: { done: true } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const result = await harness.registered.get('launch_app').execute(
    { app: 'notepad.exe' },
    { agent: { id: 's', session }, arguments: { app: 'notepad.exe' } },
  )
  assert.equal(result.value.done, true)
  assert.equal(asked, 1, 'a normal session must still ask the user once')
})

test('MCP-02 health surfaces the overlay diagnostics when the helper reports them', async () => {
  const harness = makeHarness({
    onCall: name => (name === 'diagnostic_state'
      ? { ok: true, value: { overlayState: { visible: true, displayComposition: true, pill: { pushes: 3 } } } }
      : { ok: true, value: {} }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const health = await harness.registered.get('computer_use_health').execute({}, { agent: { id: 's' } })
  assert.equal(health.overlay.displayComposition, true)
  assert.equal(health.overlay.pill.pushes, 3)
})

test('PSG-5 requiredFor documentation gate blocks CDP until the reference is read', async () => {
  const harness = makeHarness({ browserTools: [
    { name: 'tab_cdp_call', description: 'cdp', parameters: { type: 'object', properties: {} } },
  ] })
  await applyTools(harness.ctx, ToolConfig({ surfaces: ['browser'] }))
  const tool = harness.registered.get('tab_cdp_call')
  assert.ok(tool, 'browser catalog must register when surfaces=[browser]')
  assert.deepEqual(REQUIRED_DOCS.tab_cdp_call, ['computer-use-browser/references/confirmations.md'])
  await assert.rejects(
    () => tool.execute({ method: 'Runtime.evaluate' }, { agent: { id: 's' }, arguments: {} }),
    /requiredFor gate/,
  )
  harness.emitResult({ name: 'read', arguments: { file_path: 'C:/skills/computer-use-browser/references/confirmations.md' } }, { isError: false })
  const result = await tool.execute({ method: 'Runtime.evaluate' }, { agent: { id: 's' }, arguments: {} })
  assert.equal(result.value.called, 'tab_cdp_call')
})

test('PSG-9 the whole plugin plane ships no TTL contract', () => {
  // The official window2 contract has no time-based observation expiry, so no
  // model-visible surface, manifest or config key may teach one. \bttl catches
  // `TTL`, `ttlMs` and `TTL:` without matching words such as "settled".
  const surfaces = [
    'src/tool.js',
    'src/prompt.js',
    'src/index.js',
    'src/sidecar.js',
    'cordis.patch.yml',
    'dsh-plugin.json',
    'docs/plugin-readme.zh.md',
    'skills/computer-use/SKILL.md',
    'skills/computer-use-browser/SKILL.md',
  ]
  for (const rel of surfaces) {
    const text = fs.readFileSync(path.join(pluginRoot, rel), 'utf8')
    assert.equal(/\bttl/i.test(text), false, rel + ' must not mention a TTL')
  }
  const host = HostConfig({})
  assert.equal(
    Object.prototype.hasOwnProperty.call(host, 'ttlMs'),
    false,
    'the dead ttlMs knob must be gone from the host config',
  )
})

test('PSG-4 both official browser policy documents are declared, shipped and byte-identical', (t) => {
  const dir = path.join(pluginRoot, 'skills', 'computer-use-browser', 'references')
  const skill = fs.readFileSync(path.join(pluginRoot, 'skills', 'computer-use-browser', 'SKILL.md'), 'utf8')
  const codexHomeDir = process.env.CODEX_HOME || path.join(os.homedir(), '.codex')
  const official = path.join(codexHomeDir, 'plugins', 'cache', 'openai-bundled', 'browser', '26.903.61454', 'docs')
  for (const name of BROWSER_REFERENCE_FILES) {
    const ours = path.join(dir, name)
    assert.ok(fs.existsSync(ours), 'missing ' + ours)
    assert.ok(fs.statSync(ours).size > 1000, name + ' looks truncated')
    assert.match(skill, new RegExp('references/' + name.replace('.', '\\.')), 'SKILL.md must declare references/' + name)
    if (fs.existsSync(official)) {
      assert.equal(
        fs.readFileSync(ours, 'utf8'),
        fs.readFileSync(path.join(official, name), 'utf8'),
        name + ' must be the official document verbatim',
      )
    } else if (t && typeof t.skip === 'function') {
      t.skip('official browser cache not present at ' + official)
    }
  }
})

test('PSG-4 the health documentation channel lists the browser references', () => {
  const docs = documentationSummary()
  assert.deepEqual(docs.browser.references, BROWSER_REFERENCE_FILES)
  assert.equal(docs.browser.status.ok, true, 'browser references must be present')
  assert.deepEqual(docs.browser.status.missing, [])
  assert.ok(docs.browser.requiredFor['computer-use-browser/references/confirmations.md'].includes('tab_cdp_call'))
  assert.deepEqual(browserReferenceStatus().present.sort(), [...BROWSER_REFERENCE_FILES].sort())
})

test('PSG-4 the browser requiredFor gate is path-qualified, not basename-keyed', async () => {
  // The desktop and browser skills both ship `references/confirmations.md` with
  // DIFFERENT policy text, so a basename match let the Windows policy open the
  // browser CDP gate. Both directions are asserted here.
  assert.equal(
    documentWasRead([normalizeDocPath('C:/x/skills/computer-use/references/confirmations.md')], REQUIRED_DOCS.tab_cdp_call[0]),
    false,
    'the Windows confirmations policy must not satisfy the browser gate',
  )
  assert.equal(
    documentWasRead([normalizeDocPath('C:\\u\\.dsh\\skills\\computer-use-browser\\references\\confirmations.md')], REQUIRED_DOCS.tab_cdp_call[0]),
    true,
  )
  const harness = makeHarness({ browserTools: [
    { name: 'tab_cdp_call', description: 'cdp', parameters: { type: 'object', properties: {} } },
  ] })
  await applyTools(harness.ctx, ToolConfig({ surfaces: ['browser'] }))
  const tool = harness.registered.get('tab_cdp_call')
  harness.emitResult({ name: 'read', arguments: { file_path: 'C:\\x\\skills\\computer-use\\references\\confirmations.md' } }, { isError: false })
  await assert.rejects(
    () => tool.execute({ method: 'Runtime.evaluate' }, { agent: { id: 's' }, arguments: {} }),
    /requiredFor gate/,
  )
  harness.emitResult({ name: 'read', arguments: { file_path: 'C:\\x\\skills\\computer-use-browser\\references\\confirmations.md' } }, { isError: false })
  const ok = await tool.execute({ method: 'Runtime.evaluate' }, { agent: { id: 's' }, arguments: {} })
  assert.equal(ok.value.called, 'tab_cdp_call')
})

test('PSG-6 the SKILL tree grammar matches helper-rs/src/uia.rs', () => {
  const uia = fs.readFileSync(path.join(repoRoot, 'helper-rs', 'src', 'uia.rs'), 'utf8')
  const skill = fs.readFileSync(path.join(pluginRoot, 'skills', 'computer-use', 'SKILL.md'), 'utf8')

  // 1. the implementation's own template, extracted from the format! call.
  const template = uia.match(/"[^"]*\{indent\}[^"]*"/)
  assert.ok(template, 'could not find the uia.rs element-line template')
  assert.match(
    template[0],
    /\{indent\}\{\} \{\}\{state_block\}\{name\}\{suffix\}/,
    'formatter template changed: ' + template[0],
  )
  assert.match(uia, /let state_block = if pre\.is_empty\(\)/, 'state_block construction not found')

  // 2. the skill's grammar line must place the state block BEFORE the name.
  const grammarLine = skill.split('\n').find(line => line.includes('<TABs>'))
  assert.ok(grammarLine, 'SKILL.md must state the element-line grammar')
  const iRole = grammarLine.indexOf('<role>')
  const iState = grammarLine.indexOf('(<state>)')
  const iName = grammarLine.indexOf('[<name>]')
  assert.ok(iRole !== -1, 'grammar must name the role: ' + grammarLine)
  assert.ok(iState > iRole, 'the state block must sit after the role: ' + grammarLine)
  assert.ok(iName > iState, 'the name must come after the state block: ' + grammarLine)
  assert.equal(/\{x, y, width, height\}/.test(skill), false, 'bounds are not printed by the helper')
  assert.equal(/\[index\] role/.test(skill), false, 'the index is bare, not bracketed')

  // 3. the official suffix order is fixed by the code; the skill must match it.
  let cursor = -1
  for (const token of ['Description: ...', 'Value: ...', 'Secondary Actions: ...', 'ID: ...']) {
    const at = skill.indexOf(token)
    assert.ok(at > cursor, 'SKILL.md must list suffix ' + token + ' in the helper order')
    cursor = at
  }

  // 4. role casing and whitespace collapsing must be described the way the code does.
  const richLowercases = /raw\.trim\(\)\.to_ascii_lowercase\(\)/.test(uia)
  assert.equal(
    /default profile[^.]*lower-cases/i.test(skill),
    richLowercases,
    'SKILL.md role-casing claim must match uia.rs',
  )
  const officialCollapses = /collapse_whitespace\(&name\)/.test(uia)
  assert.equal(
    /official\s*`?\s*profile[^.]*collapses runs of whitespace/i.test(skill),
    officialCollapses,
    'SKILL.md whitespace claim must match uia.rs',
  )
  assert.ok(/SplitButton/.test(skill), 'SKILL.md must show the case-preserved role example')
})

test('PSG-3 the plugin plane no longer teaches a window2 diff contract', () => {
  for (const rel of ['skills/computer-use/SKILL.md', 'skills/computer-use-browser/SKILL.md', 'src/tool.js', 'src/prompt.js']) {
    const text = fs.readFileSync(path.join(pluginRoot, rel), 'utf8')
    assert.equal(
      /disableDiffing|disable_diffing|no accessibility-tree change/i.test(text),
      false,
      rel + ' must not teach the invented window2 diff contract',
    )
  }
  const readme = fs.readFileSync(path.join(pluginRoot, 'docs', 'plugin-readme.zh.md'), 'utf8')
  assert.equal(/无障碍树增量|默认返回相对上次/.test(readme), false, 'README must not claim incremental trees')
})

test('PSG-7 batch_actions is documented as conditional, never unconditionally', () => {
  const skill = fs.readFileSync(path.join(pluginRoot, 'skills', 'computer-use', 'SKILL.md'), 'utf8')
  const start = skill.indexOf('`batch_actions`')
  assert.ok(start !== -1, 'SKILL.md must document batch_actions')
  const paragraph = skill.slice(start).split('\n\n')[0]
  assert.match(paragraph, /element_index/, 'the batch rule must be limited to actions that do not consume element_index')
  assert.match(paragraph, /invalidates every index|do \*\*not\*\* depend/i, 'the batch rule must say why element indexes make batching unsafe')
})

test('APS-10 the official approvalRequest parser and refusal display name', async () => {
  // Verbatim from helper_transport.js: an approval exists only when app is a
  // non-empty trimmed string; displayName falls back to the trimmed app; riskLevel
  // is the two-valued low|high mapping; allowPersistentApproval defaults true.
  assert.equal(normalizeApprovalRequest(undefined), null)
  assert.equal(normalizeApprovalRequest({}), null)
  assert.equal(normalizeApprovalRequest({ app: '   ' }), null)
  assert.deepEqual(normalizeApprovalRequest({ app: '  mspaint.exe  ', displayName: '  Paint ' }), {
    app: 'mspaint.exe',
    displayName: 'Paint',
    allowPersistentApproval: true,
  })
  assert.deepEqual(normalizeApprovalRequest({ app: 'notepad.exe', riskLevel: 'medium', allowPersistentApproval: false }), {
    app: 'notepad.exe',
    displayName: 'notepad.exe',
    allowPersistentApproval: false,
  })
  assert.equal(normalizeApprovalRequest({ app: 'x.exe', riskLevel: 'high' }).riskLevel, 'high')

  let seen
  const exePath = 'C:\\Program Files\\Paint\\mspaint.exe'
  const harness = makeHarness({
    approval: { request: async request => { seen = request; return 'rejected' } },
    onCall: () => ({
      ok: false,
      error: 'AppApprovalRequired',
      approvalRequest: { app: exePath, displayName: '  Paint  ', riskLevel: 'medium', allowPersistentApproval: true },
    }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('click')
  await assert.rejects(
    () => tool.execute({ window: { app: exePath, id: 1 }, x: 1, y: 1 }, { agent: { id: 's' }, arguments: { app: exePath } }),
    error => {
      assert.match(String(error.message), /not approved to use Paint$/)
      assert.equal(String(error.message).includes('Program Files'), false, 'the refusal must not leak the exe path')
      return true
    },
  )
  assert.equal(seen.reason, 'Allow Computer Use to use Paint?')
  assert.equal(seen.riskLevel, 'low', 'an unknown riskLevel normalises to the official low')
  assert.deepEqual(seen.tool_params, { app: exePath })
  assert.deepEqual(seen.tool_params_display, [{ name: 'app', display_name: 'App', value: 'Paint' }])
})

test('APS-09 stop_audio_recording derives bytes and the official data_url from disk', async () => {
  // Official Audio.d.ts = { filepath, bytes, data_url }; the official Windows
  // client re-reads the WAV the helper wrote. DSH's helper returns filepath +
  // data_url, so the plugin must add the missing byte count here.
  assert.throws(
    () => deriveAudioResult({ ok: true, value: null }, 'stop_audio_recording'),
    /did not return computer audio/,
  )
  assert.throws(
    () => deriveAudioResult({ ok: true, value: { filepath: '' } }, 'stop_audio_recording'),
    /did not return a computer audio filepath/,
  )
  assert.deepEqual(
    deriveAudioResult({ ok: true, value: { untouched: true } }, 'start_audio_recording').value,
    { untouched: true },
    'start_audio_recording is passed through, exactly like the official client',
  )

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cu-audio-'))
  const file = path.join(dir, 'clip.wav')
  const wav = Buffer.concat([Buffer.from('RIFF'), Buffer.alloc(44), Buffer.from([1, 2, 3, 4])])
  fs.writeFileSync(file, wav)
  try {
    const harness = makeHarness({
      desktopTools: DESKTOP_13.concat([
        { name: 'stop_audio_recording', description: 'stop', parameters: { type: 'object', properties: {} } },
      ]),
      // A stale data_url from the helper must be replaced by the bytes on disk.
      onCall: () => ({ ok: true, value: { filepath: file, data_url: 'data:audio/wav;base64,STALE' } }),
    })
    await applyTools(harness.ctx, ToolConfig({}))
    const tool = harness.registered.get('stop_audio_recording')
    assert.ok(tool, 'stop_audio_recording must be registered')
    const result = await tool.execute({}, { agent: { id: 's' }, arguments: {} })
    assert.equal(result.value.filepath, file)
    assert.equal(result.value.bytes, wav.length, 'bytes must equal the WAV file size')
    assert.equal(result.value.bytes, fs.statSync(file).size)
    assert.equal(result.value.data_url, 'data:audio/wav;base64,' + wav.toString('base64'))
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('APS-09 a missing WAV fails loudly with the official string', async () => {
  const harness = makeHarness({
    desktopTools: DESKTOP_13.concat([
      { name: 'stop_audio_recording', description: 'stop', parameters: { type: 'object', properties: {} } },
    ]),
    onCall: () => ({ ok: true, value: { filepath: path.join(os.tmpdir(), 'cu-does-not-exist.wav') } }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('stop_audio_recording')
  await assert.rejects(
    () => tool.execute({}, { agent: { id: 's' }, arguments: {} }),
    error => {
      assert.match(String(error.message), /did not return a computer audio filepath/)
      assert.ok(error.cause, 'the read failure must not be swallowed (APS-09)')
      return true
    },
  )
})


test('PKG-01..05 the plugin manifest declares identity, skills, capabilities and turn hooks', () => {
  const manifestPath = path.join(pluginRoot, 'dsh-plugin.json')
  assert.ok(fs.existsSync(manifestPath), 'dsh-plugin.json missing')
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
  for (const key of ['name', 'version', 'description', 'author', 'license', 'capabilities', 'skills', 'interface', 'hooks', 'tools']) {
    assert.ok(manifest[key] !== undefined, 'manifest missing ' + key)
  }
  assert.equal(manifest.skills, './skills/')
  assert.ok(Array.isArray(manifest.capabilities) && manifest.capabilities.includes('Interactive'))
  for (const hook of ['Stop', 'Interrupt', 'SubagentStop']) {
    assert.equal(manifest.hooks[hook], TURN_LIFECYCLE_HOOKS[hook])
  }
  assert.ok(manifest.tools.enabled.includes('batch_actions'))
  assert.ok(manifest.tools.startup_timeout_ms > 0)
  assert.ok(Array.isArray(manifest.tools.env_allowlist) && manifest.tools.env_allowlist.length > 0)
  assert.deepEqual(manifest.tools.env_allowlist, BASE_ENV_ALLOWLIST, 'manifest env allowlist must match the shipped one')
  for (const entry of fs.readdirSync(path.join(pluginRoot, 'skills'))) {
    const skill = path.join(pluginRoot, 'skills', entry, 'SKILL.md')
    assert.ok(fs.existsSync(skill), 'skills/' + entry + ' has no SKILL.md')
    const text = fs.readFileSync(skill, 'utf8')
    assert.match(text, /^---/)
    assert.match(text, /name:/)
    assert.match(text, /description:/)
  }
  assert.deepEqual(manifest.capabilities, SERVICE_CAPABILITIES)
})

test('host and tool configs validate and default to the official surface', () => {
  const host = HostConfig({})
  assert.equal(host.surface, 'computer')
  assert.ok(host.startupTimeoutMs > 0)
  assert.deepEqual(host.envAllowlist, [])
  const tool = ToolConfig({})
  assert.deepEqual(tool.surfaces, ['computer'])
  assert.equal(tool.approvalDefault, 'prompt')
  assert.deepEqual(tool.enabledTools, ['batch_actions', 'scroll_element', 'fill_form'])
  assert.deepEqual(tool.approvalTools, {})
})

test('overlay capture exclusion: on-screen-safe by default and wired to the helper', () => {
  // The pill must be visible to the operator on every machine. The affinity mode is the
  // one that silently blanked the DirectComposition content on screen (2026-09-15), so it
  // can never be the default and an unknown value must not fall through to it.
  const host = HostConfig({})
  assert.equal(host.overlayCaptureExclusion, 'mask', 'the pill stays on screen by default')
  assert.equal(HostConfig({ overlayCaptureExclusion: 'wda' }).overlayCaptureExclusion, 'wda')
  assert.equal(HostConfig({ overlayCaptureExclusion: 'off' }).overlayCaptureExclusion, 'off')
  assert.throws(() => HostConfig({ overlayCaptureExclusion: 'affinity' }), Error)
  // The plugin writes the mode into the environment and the Rust helper reads it; a
  // rename on one side would silently fall back to a helper default, which is exactly the
  // class of drift that hid the pill.
  const sidecar = source('sidecar.js')
  const overlay = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'overlay', 'mod.rs'), 'utf8')
  assert.match(sidecar, /DSH_CU_OVERLAY_CAPTURE_EXCLUSION/)
  assert.match(overlay, /DSH_CU_OVERLAY_CAPTURE_EXCLUSION/)
  assert.match(sidecar, /overlayCaptureExclusion/)
})

test('list_apps gets a catalog-sized transport budget below the harness budget', () => {
  // The official transport budgets every request 10 s and kills the helper when it
  // expires. Two things follow for the one method that builds the installed-app catalog:
  // the budget must be larger than 10 s (a kill throws the warm catalog away), and it must
  // stay below DSH's own 25 s tool-call budget (an abort is heavier than a timeout -- it
  // used to latch the turn as "stopped by the user with the physical Escape key", which
  // ended turns the operator never stopped).
  const HARNESS_TOOL_BUDGET_MS = 25000
  const host = HostConfig({})
  assert.equal(host.listAppsTimeoutMs, 20000)
  assert.ok(host.listAppsTimeoutMs > host.timeoutMs, 'above the official 10 s transport budget')
  assert.ok(host.listAppsTimeoutMs < HARNESS_TOOL_BUDGET_MS, 'below the harness tool budget')
  const sidecar = new Sidecar(host)
  assert.equal(sidecar.timeoutFor('call', { name: 'list_apps' }), 20000)
  assert.equal(sidecar.timeoutFor('call', { name: 'list_apps' }), host.listAppsTimeoutMs)
  assert.equal(new Sidecar(HostConfig({ listAppsTimeoutMs: 5000 })).timeoutFor('call', { name: 'list_apps' }), 5000, 'used verbatim')
  assert.equal(sidecar.timeoutFor('call', { name: 'launch_app' }), host.launchAppTimeoutMs)
  assert.equal(sidecar.timeoutFor('call', { name: 'click' }), 10000, 'every other call keeps the official budget')
  assert.equal(sidecar.timeoutFor('health'), 10000)
  assert.equal(new Sidecar(HostConfig({ listAppsTimeoutMs: 90000 })).timeoutFor('call', { name: 'list_apps' }), 90000)
  // A catalog build that outruns its budget must not cost the warm catalog: the sidecar
  // keeps the helper alive for this one method.
  const sidecarSource = source('sidecar.js')
  assert.match(sidecarSource, /keepAlive/)
  // The helper-side evidence path must exist too: the transport kills the helper on a
  // timeout, so the only surviving record of a slow request is the helper's own log.
  const helperSource = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'main.rs'), 'utf8')
  assert.match(helperSource, /slow-requests\.log/)
  assert.match(helperSource, /note_slow_request/)
  // And the request path must not resolve 751 product names: that is what made one
  // list_apps take 31.5 s inside a request (slow-requests.log, 2026-09-15).
  const catalogSource = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'app_catalog.rs'), 'utf8')
  assert.match(catalogSource, /fn installed_display_name/)
  assert.match(catalogSource, /installed_display_name\(&shell\.display_name, &shell\.product_name\)/)
  assert.doesNotMatch(catalogSource, /prefer_product_name\(&shell\.display_name/)
})

test('get_window_state and type_text get their own budgets, and a timeout names the cold start', () => {
  // AX-WEDGE: these two calls were the ones whose accessibility work outran the base
  // budget on heavy Electron windows (QQNT main window, 2026-10-25), and every
  // base-budget timeout KILLED the helper. The helper now answers within its budget
  // (degraded tree, select-all-guarded retry), so the knobs only decide how long a
  // healthy call may run; when the kill does happen, the error must name the cold
  // start so the model reads a slower retry as the cost of the kill, not a second
  // failure.
  const HARNESS_TOOL_BUDGET_MS = 25000
  const host = HostConfig({ windowStateTimeoutMs: 24000, typeTextTimeoutMs: 24000 })
  assert.ok(host.windowStateTimeoutMs < HARNESS_TOOL_BUDGET_MS, 'below the harness tool budget')
  assert.ok(host.typeTextTimeoutMs < HARNESS_TOOL_BUDGET_MS, 'below the harness tool budget')
  const sidecar = new Sidecar(host)
  assert.equal(sidecar.timeoutFor('call', { name: 'get_window_state' }), 24000, 'used verbatim')
  assert.equal(sidecar.timeoutFor('call', { name: 'type_text' }), 24000, 'used verbatim')
  // 0/absent falls back to the base budget (the helper answers degraded inside it now).
  const base = new Sidecar(HostConfig({}))
  assert.equal(base.timeoutFor('call', { name: 'get_window_state' }), base.config.timeoutMs)
  assert.equal(base.timeoutFor('call', { name: 'type_text' }), base.config.timeoutMs)
  // The kill-on-timeout error names the cold-start consequence, and the kill is
  // counted for computer_use_health (a rising count points at the call class whose
  // budget needs tuning).
  const sidecarSource = source('sidecar.js')
  assert.match(sidecarSource, /cold-starts the helper/)
  assert.match(sidecarSource, /killedOnTimeout/)
  assert.match(source('index.js'), /killStats\(\)/)
})

test('an aborted tool call cancels the action instead of latching the turn as user-stopped', () => {
  // The harness aborts a tool call when its own 25 s budget expires. The abort path used
  // to send the `interrupt` RPC, which latches the helper as stopped; every later call in
  // that process then reported "Computer Use was stopped by the user with the physical
  // Escape key" and the model ended the turn (session b9bdf958, operator pressed nothing).
  const sidecarSource = source('sidecar.js')
  const abortBlock = /const onAbort = \(\) => \{([\s\S]*?)\n      \}/.exec(sidecarSource)
  assert.ok(abortBlock, 'the abort handler must be findable')
  assert.match(abortBlock[1], /method: 'cancel'/)
  assert.doesNotMatch(abortBlock[1], /method: 'interrupt'/)
  // The host-driven Stop hook keeps the latching RPC: that one really is the user stopping.
  const indexSource = source('index.js')
  assert.match(indexSource, /return this\.sidecar\.request\('interrupt'\)/)
})

test('D-E maxImageEdge defaults to the official no-cap behaviour and stays labelled', () => {
  const host = HostConfig({})
  // Decision D-E: parity first. 0 ships the screenshot the official helper ships; a
  // positive value is an opt-in DSH downscale for token saving and must stay visible in
  // the health payload rather than being an undocumented silent cap.
  assert.equal(host.maxImageEdge, 0, 'the default is the official behaviour (no cap)')
  const limits = captureLimits(host.maxImageEdge, repoRoot)
  assert.equal(limits.maxImageEdge.value, 0)
  assert.equal(limits.maxImageEdge.official, null)
  assert.equal(limits.maxImageEdge.kind, 'dsh-extension')
  assert.match(limits.maxImageEdge.note, /DSH-only/)
  const capped = captureLimits(1280, repoRoot)
  assert.equal(capped.maxImageEdge.value, 1280, 'the knob still works when set')
})

test('MCP-02/07/12 health exposes the server instruction, catalog, docs and env audit', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const health = await harness.registered.get('computer_use_health').execute({}, { agent: { id: 's' } })
  assert.equal(health.codexRequired, false)
  assert.equal(health.backend, 'fake')
  assert.ok(health.tools.exposed.includes('get_window_state'))
  assert.ok(health.tools.exposed.includes('scroll_element'), 'F4: scroll_element must be discoverable in the model-facing catalog')
  assert.ok(health.tools.exposed.includes('fill_form'), 'fill_form must be discoverable: multi-field forms are the main round-trip sink')
  assert.deepEqual(health.tools.enabled, ['batch_actions', 'scroll_element', 'fill_form'])
  assert.equal(health.approval.default, 'prompt')
  assert.ok(health.documentation.references.includes('guidance.md'))
  assert.equal(health.promptAssets.ok, true)
  assert.deepEqual(health.documentationGate.requiredFor.tab_cdp_call, ['computer-use-browser/references/confirmations.md'])
})


test('TURN-1 turn/end sends end_turn with the real turn id, then releases the overlay', async () => {
  const { Context } = await import('@deepseek-ai/cordis')
  const ctx = new Context()
  await ctx.plugin(ComputerUseService, {})
  const service = ctx.dshComputerUse
  const ended = []
  service.endTurn = params => { ended.push(params); return Promise.resolve({ ok: true }) }
  let releases = 0
  service.releaseOverlay = () => { releases += 1; return Promise.resolve({ ok: true }) }
  service.lastConversationId = 'session-abc'
  ctx.emit('session/event', { id: 'session-abc' }, { type: 'turn/end', data: { turn: 7, reason: { kind: 'completed' } } })
  await Promise.resolve()
  assert.deepEqual(ended, [{ session_id: 'session-abc', turn_id: '7' }], 'official turn_ended payload')
  assert.equal(releases, 1)
})

test('MCP-07 enabledTools controls which harness extensions enter the catalog', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({ enabledTools: ['diagnostic_state'] }))
  const names = [...harness.registered.keys()]
  assert.ok(names.includes('diagnostic_state'))
  assert.equal(names.includes('batch_actions'), false, 'batch_actions is not enabled by this config')
})

// ── Form-fill / IME / activation ergonomics (2026-10-19) ──────────────────────

test('OPT-1 fill_form is a native call and must not be routed to the Python catalog', async () => {
  const { usesPython } = await import('../src/sidecar.js')
  assert.equal(usesPython('call', { name: 'fill_form', arguments: { window: {}, fields: [] } }), false)
  // A batch that contains fill_form must stay native too.
  assert.equal(
    usesPython('call', {
      name: 'batch_actions',
      arguments: { actions: [{ name: 'fill_form', arguments: { fields: [] } }] },
    }),
    false,
  )
})

test('OPT-1 the helper implements fill_form with replace semantics', () => {
  const main = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'main.rs'), 'utf8')
  assert.match(main, /fn fill_form\(state: &Mutex<HelperState>, params: &Map<String, Value>\)/)
  assert.match(main, /"fill_form" => \{/)
  // The coordinate fallback must select-all + paste; a bare paste is the bug.
  assert.match(main, /input::replace_text_for\(/)
  assert.doesNotMatch(main, /input::paste_text\(&value\)\?/)
  // Audit P2-4 (2026-10-25): the input fallback moved OFF the UIA monitor
  // thread (synthetic typing there queued every other UIA request). The engine
  // reports the miss; main.rs performs the replace on the caller's thread —
  // so uia.rs must now contain NO input-injection calls at all.
  const uia = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'uia.rs'), 'utf8')
  assert.doesNotMatch(uia, /crate::input::/)
  assert.doesNotMatch(uia, /crate::input::paste_text\(value\)/)
})

test('OPT-2 fill_form is discoverable in the model-facing catalog', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  assert.ok(harness.registered.has('fill_form'))
  const names = [...harness.registered.keys()]
  assert.ok(names.includes('fill_form'), 'a 2+ field form must be fillable in one call')
})

test('OPT-3 synthetic keyboard input installs the IME guard on the TARGET window', () => {
  const input = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'input.rs'), 'utf8')
  const ime = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'ime.rs'), 'utf8')
  // The guard must be installed for the window that receives the keystrokes:
  // the keyboard layout is per-thread, so an in-process switch is a no-op.
  assert.match(
    input,
    /crate::ime::ImeGuard::install_for\(hwnd\)/,
    'press_key/type_unicode must suppress the IME on the target window',
  )
  // Strip the module docs: they legitimately mention the rejected approach.
  const code = ime
    .split(/\r?\n/)
    .filter(line => !line.trimStart().startsWith('//'))
    .join('\n')
  assert.match(code, /ImmAssociateContext/, 'the target IME context must be detached')
  assert.match(code, /WM_INPUTLANGCHANGEREQUEST/, 'the target thread layout is switched by message')
  assert.doesNotMatch(code, /ActivateKeyboardLayout/, 'per-process switching does not reach the target')
  assert.match(code, /impl Drop for ImeGuard/, 'the guard restores the previous state')
  assert.match(code, /pub fn install_for\(hwnd: Option<isize>\)/)
})

test('OPT-4 activate_window bumps Z-order before asking for focus', () => {
  const enum_ = fs.readFileSync(path.join(pluginRoot, 'helper-rs', 'src', 'enum_windows.rs'), 'utf8')
  assert.match(enum_, /fn bump_to_top\(hwnd: HWND\)/)
  assert.match(enum_, /HWND_TOPMOST/)
  assert.match(enum_, /HWND_NOTOPMOST/)
  assert.ok(
    enum_.indexOf('bump_to_top(hwnd);') < enum_.indexOf('SetForegroundWindow(hwnd).as_bool()'),
    'Z-order must be fixed before SetForegroundWindow, otherwise the bump is undone',
  )
})

test('P1-6 admitImages harvests top-level images and calls attachments service correctly', async () => {
  const saved = []
  const attachments = {
    saveImages: async (items) => {
      saved.push(...items)
      return items.map((_, i) => ({ type: 'image', id: 'img-' + i }))
    },
  }
  const dummyB64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
  const harness = makeHarness({
    attachments,
    onCall: async () => ({
      ok: true,
      value: { screenshots: [{ id: 's1' }] },
      images: [{ data: dummyB64, mimeType: 'image/png', name: 'tab_shot.png' }],
    }),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('get_window_state')
  const result = await tool.execute({ window: { app: 'test', id: 1 } }, { agent: { id: 's' } })
  assert.equal(saved.length, 1, 'attachments.saveImages must be called once')
  assert.equal(saved[0].name, 'tab_shot.png')
  assert.equal(saved[0].mediaType, 'image/png')
  assert.ok(result.images.find(b => b.type === 'image' && b.id === 'img-0'), 'result.images must contain the admitted vision ImageBlock')
})


// ---------------------------------------------------------------------------
// `text` is a payload on most tools and an element selector on three of them.
// The selector pass used to run for every tool and then delete `input.text`, so
// `type_text({window, text})` reached the helper as `{window}` and failed with
// "text is required" (helper-rs protocol.rs require_text_allow_empty). These two
// tests pin both halves of the split.

test('C-TEXT-1 type_text forwards its text payload instead of consuming it as a selector', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('type_text')
  assert.ok(tool, 'type_text must be registered on the computer surface')

  await tool.execute(
    { window: { app: 'notepad.exe', id: 1 }, text: 'hello world' },
    { agent: { id: 's' }, arguments: {} },
  )

  const call = harness.calls.find(entry => entry.name === 'type_text')
  assert.ok(call, 'the helper must be called')
  assert.equal(call.args.text, 'hello world', 'the payload must survive the selector pass')
  assert.equal(call.args.element_index, undefined, 'type_text must not gain an element_index')
})

test('C-TEXT-2 click still consumes text as an element selector', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('click')
  assert.ok(tool, 'click must be registered')

  await tool.execute(
    { window: { app: 'notepad.exe', id: 1 }, text: 'Save', x: 10, y: 10 },
    { agent: { id: 's' }, arguments: {} },
  )

  const call = harness.calls.find(entry => entry.name === 'click')
  assert.ok(call, 'the helper must be called')
  assert.equal(call.args.text, undefined, 'text is a selector for click, never a payload')
  assert.equal(call.args.x, 10, 'coordinates still reach the helper')
})

// tools-improved #23: set_value with the value in `text` (and no value/index)
// must say where it belongs, not fail with a bare "value is required".
test('C-TEXT-5 set_value points a misplaced value at value', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('set_value')
  assert.ok(tool, 'set_value must be registered')

  let threw = null
  try {
    await tool.execute(
      { window: { app: 'magpie.exe', id: 2 }, text: 'kilo' },
      { agent: { id: 's' }, arguments: {} },
    )
  } catch (e) {
    threw = e
  }
  assert.ok(threw, 'a value in text with no value must throw')
  assert.match(String(threw.message), /needs its value in "value", not "text"/)
  assert.equal(harness.calls.find(entry => entry.name === 'set_value'), undefined)
})

test('C-TEXT-4 click text with no match names the miss', async () => {
  const harness = makeHarness()
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('click')
  assert.ok(tool, 'click must be registered')

  let threw = null
  try {
    await tool.execute(
      { window: { app: 'magpie.exe', id: 2 }, text: 'Definitely Not Present' },
      { agent: { id: 's' }, arguments: {} },
    )
  } catch (e) {
    threw = e
  }
  assert.ok(threw, 'an unmatched text must throw, not call the helper without a target')
  assert.match(String(threw.message), /found no element matching text "Definitely Not Present"/)
  assert.equal(harness.calls.find(entry => entry.name === 'click'), undefined, 'the helper is not called')
})

test('C-TEXT-3 session_note keeps its text payload', async () => {
  const harness = makeHarness({
    desktopTools: DESKTOP_13.concat([{
      name: 'session_note',
      description: 'note',
      parameters: { type: 'object', properties: { text: { type: 'string' } } },
    }]),
  })
  await applyTools(harness.ctx, ToolConfig({}))
  const tool = harness.registered.get('session_note')
  assert.ok(tool, 'session_note must be registered when the catalog offers it')

  await tool.execute({ text: 'remember this' }, { agent: { id: 's' }, arguments: {} })

  const call = harness.calls.find(entry => entry.name === 'session_note')
  assert.ok(call, 'the helper must be called')
  assert.equal(call.args.text, 'remember this', 'a note is a payload, not a selector')
})


