import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { Context } from '@deepseek-ai/cordis'
import ComputerUseService, { Config as HostConfig } from '../src/index.js'
import { Config as ToolConfig, apply as applyTools } from '../src/tool.js'
import { nativeHelperCandidates } from '../src/paths.js'
import {
  Sidecar,
  usesPython,
  engineFor,
  pythonCatalog,
  pythonSurfaceFor,
  browserChannelMode,
  PYTHON_SURFACES,
  LINUX_CALLS,
  WINDOW2_CALLS,
  WINDOW2_EXTENSION_CALLS,
  WAIT_FOR_DEFAULT_TIMEOUT_MS,
  WAIT_FOR_MAX_TIMEOUT_MS,
  WAIT_FOR_METHOD,
  WAIT_FOR_TOOL_NAME,
  callParamsFor,
  isWindow2Call,
  isWindow2Surface,
} from '../src/sidecar.js'

const pluginRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const stubHelper = path.join(pluginRoot, 'scripts', 'stub-linux-helper.mjs')
/**
 * Test double for the Python engine. The real one is a Python process whose browser
 * surface needs Chromium/CDP; the double speaks the same JSONL protocol and records the
 * argv it was spawned with, which is what proves the browser channel was opened with
 * `--surface browser` and not with the native face.
 */
const stubPython = path.join(pluginRoot, 'scripts', 'stub-python-engine.sh')

const EXPECTED_7_TOOLS = [
  'list_apps',
  'get_app_state',
  'screenshot',
  'click',
  'scroll',
  'press_key',
  'type_text',
]

test('P1-PATHS linux candidate paths use helper-linux/ and support DSH_COMPUTER_USE_HELPER override', () => {
  const origPlatform = process.platform
  const origEnv = process.env.DSH_COMPUTER_USE_HELPER

  try {
    delete process.env.DSH_COMPUTER_USE_HELPER
    const candidates = nativeHelperCandidates(pluginRoot)
    if (process.platform === 'linux') {
      assert.ok(
        candidates.some(p => p.includes(path.join('helper-linux', 'target', 'release', 'dsh-computer-use'))),
        'must include helper-linux release target',
      )
      assert.ok(
        candidates.some(p => p.includes(path.join('helper-linux', 'target', 'debug', 'dsh-computer-use'))),
        'must include helper-linux debug target',
      )
      assert.ok(
        candidates.some(p => p.includes(path.join('helper-linux', 'bin', 'linux-x64', 'dsh-computer-use'))),
        'must include helper-linux bin linux-x64 target',
      )
      assert.ok(
        candidates.some(p => p === path.join(pluginRoot, 'dsh-computer-use')),
        'must include repo-root dsh-computer-use fallback',
      )
    }

    // Test DSH_COMPUTER_USE_HELPER override
    process.env.DSH_COMPUTER_USE_HELPER = '/custom/path/to/helper'
    const overridden = nativeHelperCandidates(pluginRoot)
    assert.equal(overridden[0], '/custom/path/to/helper', 'DSH_COMPUTER_USE_HELPER must be first candidate')
  } finally {
    if (origEnv !== undefined) process.env.DSH_COMPUTER_USE_HELPER = origEnv
    else delete process.env.DSH_COMPUTER_USE_HELPER
  }
})

test('P1-SIDECAR LINUX_CALLS and usesPython routing', () => {
  assert.equal(LINUX_CALLS.size, 7)
  for (const name of EXPECTED_7_TOOLS) {
    assert.ok(LINUX_CALLS.has(name), 'LINUX_CALLS must contain ' + name)
  }

  assert.equal(pythonCatalog('linux'), false, 'linux surface catalog does not use Python')
  assert.equal(usesPython('tools', { surface: 'linux' }), false)

  // Under backend='linux', all 7 tools stay on native helper
  for (const name of EXPECTED_7_TOOLS) {
    assert.equal(
      usesPython('call', { name }, 'linux'),
      false,
      name + ' must stay on native helper when backend=linux',
    )
  }

  // Under backend='windows', get_app_state and screenshot fall back to Python
  assert.equal(usesPython('call', { name: 'screenshot' }, 'windows'), true)
  assert.equal(usesPython('call', { name: 'get_app_state' }, 'windows'), true)
  assert.equal(usesPython('call', { name: 'click' }, 'windows'), false)

  // Browser calls still use Python
  assert.equal(usesPython('call', { name: 'create_tab' }, 'linux'), true)
})

test('P1-DEFAULT-SURFACE backend=linux defaults surface to linux when unspecified', () => {
  // Sidecar
  const sidecarLinux = new Sidecar({ backend: 'linux' })
  assert.equal(sidecarLinux.config.surface, 'linux')

  const sidecarExplicit = new Sidecar({ backend: 'linux', surface: 'computer' })
  assert.equal(sidecarExplicit.config.surface, 'computer')

  const sidecarWin = new Sidecar({ backend: 'windows' })
  assert.equal(sidecarWin.config.surface, undefined)

  // ComputerUseService
  const serviceLinux = new ComputerUseService(new Context(), { backend: 'linux' })
  assert.equal(serviceLinux.config.surface, 'linux')

  const serviceExplicit = new ComputerUseService(new Context(), { backend: 'linux', surface: 'custom' })
  assert.equal(serviceExplicit.config.surface, 'custom')

  const serviceWin = new ComputerUseService(new Context(), { backend: 'windows' })
  assert.equal(serviceWin.config.surface, 'computer')
})

test('P1-TOOL-REGISTRATION linux surface registers the 7 tools via stub helper', async () => {
  const registered = new Map()
  const calls = []

  const dshComputerUse = {
    config: { backend: 'linux', surface: 'linux' },
    tools: async surface => {
      assert.equal(surface, 'linux')
      return {
        surface: 'linux',
        tools: EXPECTED_7_TOOLS.map(name => ({
          name,
          description: 'Tool ' + name,
          parameters: { type: 'object', properties: {} },
        })),
      }
    },
    call: async (name, args, signal, meta) => {
      calls.push({ name, args, meta })
      return { ok: true, name, value: { done: name }, images: [] }
    },
    health: async () => ({ ok: true, codexRequired: false, backend: 'linux', surface: 'linux' }),
    releaseOverlay: async () => ({ ok: true }),
    shutdownSidecar: async () => {},
  }

  const ctx = {
    dshComputerUse,
    on() {},
    effect(fn) { fn() },
    get() { return undefined },
    tools: {
      register(tool) {
        registered.set(tool.name, tool)
      },
    },
    systemPrompt: { section() {} },
  }

  await applyTools(ctx, ToolConfig({}))

  for (const name of EXPECTED_7_TOOLS) {
    assert.ok(registered.has(name), 'missing registered tool: ' + name)
  }
  assert.ok(registered.has('computer_use_health'), 'must register health tool')
  assert.ok(registered.has('computer_use_experience'), 'must register experience tool')

  // Execute a tool and check dispatch
  const clickTool = registered.get('click')
  const res = await clickTool.execute({ app: 'linux-window:1', x: 10, y: 20 }, { agent: { id: 'test' } })
  assert.deepEqual(res.value, { done: 'click' })
  assert.equal(calls.length, 1)
  assert.equal(calls[0].name, 'click')
  assert.deepEqual(calls[0].args, { app: 'linux-window:1', x: 10, y: 20 })
})

test('P1-E2E-SMOKE sidecar end-to-end with stub-linux-helper', async () => {
  const sidecar = new Sidecar({
    backend: 'linux',
    engineRoot: pluginRoot,
  })

  // Point to stub helper
  const origEnv = process.env.DSH_COMPUTER_USE_HELPER
  process.env.DSH_COMPUTER_USE_HELPER = stubHelper

  try {
    const health = await sidecar.request('health')
    assert.equal(health.backend, 'linux')
    assert.equal(health.codexRequired, false)

    const listed = await sidecar.request('tools')
    assert.equal(listed.surface, 'linux')
    const names = listed.tools.map(t => t.name)
    assert.deepEqual(names.sort(), [...EXPECTED_7_TOOLS].sort())

    // Test get_app_state returns images
    const state = await sidecar.request('call', {
      name: 'get_app_state',
      arguments: { app: 'org.gnome.TextEditor' },
    })
    assert.equal(state.ok, true)
    assert.equal(state.images.length, 1)
    assert.ok(state.value.text.includes('TextEditor'))

    // Test screenshot returns images
    const shot = await sidecar.request('call', {
      name: 'screenshot',
      arguments: { app: 'linux-window:101' },
    })
    assert.equal(shot.ok, true)
    assert.equal(shot.images.length, 1)
    assert.equal(shot.value.width, 1920)

    // Test click
    const click = await sidecar.request('call', {
      name: 'click',
      arguments: { app: 'linux-window:101', x: 100, y: 200 },
    })
    assert.equal(click.value.action, 'click')
  } finally {
    await sidecar.request('shutdown').catch(() => {})
    sidecar.dispose()
    if (origEnv !== undefined) process.env.DSH_COMPUTER_USE_HELPER = origEnv
    else delete process.env.DSH_COMPUTER_USE_HELPER
  }
})

const EXPECTED_WINDOW2_TOOLS = [
  'list_windows',
  'get_window',
  'list_apps',
  'launch_app',
  'get_window_state',
  'click',
  'press_key',
  'type_text',
  'scroll',
  'set_value',
  'drag',
  'perform_secondary_action',
  'activate_window',
]

test('P2-ROUTING WINDOW2_CALLS definition and usesPython routing under backend=linux', () => {
  assert.equal(WINDOW2_CALLS.size, 13)
  for (const name of EXPECTED_WINDOW2_TOOLS) {
    assert.ok(WINDOW2_CALLS.has(name), 'WINDOW2_CALLS must contain ' + name)
  }

  // Under backend='linux', all 13 window2 methods stay on native helper
  for (const name of EXPECTED_WINDOW2_TOOLS) {
    assert.equal(
      usesPython('call', { name }, 'linux'),
      false,
      name + ' must stay on native helper when backend=linux',
    )
  }

  // Under backend='windows', all 13 window2 methods stay on native helper too
  for (const name of EXPECTED_WINDOW2_TOOLS) {
    assert.equal(
      usesPython('call', { name }, 'windows'),
      false,
      name + ' must stay on native helper when backend=windows',
    )
  }

  // Browser calls still use Python on both platforms
  assert.equal(usesPython('call', { name: 'create_tab' }, 'linux'), true)
  assert.equal(usesPython('call', { name: 'create_tab' }, 'windows'), true)

  // batch_actions with window2 actions stays on native helper
  assert.equal(
    usesPython('call', { name: 'batch_actions', arguments: { actions: [{ name: 'click' }, { name: 'set_value' }] } }, 'linux'),
    false,
  )
  // batch_actions with unknown action uses Python
  assert.equal(
    usesPython('call', { name: 'batch_actions', arguments: { actions: [{ name: 'create_tab' }] } }, 'linux'),
    true,
  )
})

test('P2-ROUTING the DSH wait extension rides the window2 face without joining the official thirteen', () => {
  // The official set stays exactly thirteen: its size is the parity claim, and every
  // surface test depends on it.
  assert.equal(WINDOW2_CALLS.size, 13)
  assert.equal(WINDOW2_CALLS.has(WAIT_FOR_METHOD), false, 'wait_for is not an official method')
  assert.ok(WINDOW2_EXTENSION_CALLS.has(WAIT_FOR_METHOD))
  assert.equal(isWindow2Call(WAIT_FOR_METHOD), true)
  assert.equal(isWindow2Call('click'), true)
  assert.equal(isWindow2Call('create_tab'), false)

  // On the Linux desktop backend it must reach the native helper. This is the assertion
  // that matters: a wait routed to the Python engine would be an unknown method there, and
  // the model would see a failure instead of a wait.
  assert.equal(engineFor('call', { name: WAIT_FOR_METHOD }, 'linux'), 'native')
  assert.equal(usesPython('call', { name: WAIT_FOR_METHOD }, 'linux'), false)
})

test('P2-BUDGET a wait_for request is budgeted from its own timeout, not the flat 10 s', () => {
  const sidecar = new Sidecar({ backend: 'linux', surface: 'computer' })
  const defaultBudget = sidecar.timeoutFor('call', { name: WAIT_FOR_METHOD, arguments: {} })
  const askedBudget = sidecar.timeoutFor('call', { name: WAIT_FOR_METHOD, arguments: { timeout_ms: 12000 } })
  const hugeBudget = sidecar.timeoutFor('call', { name: WAIT_FOR_METHOD, arguments: { timeout_ms: 600000 } })

  // The default wait is 5 s, so a flat 10 s budget would be *just* enough by luck; the
  // asked-for one would not be, and a transport timeout kills the helper.
  assert.ok(defaultBudget > WAIT_FOR_DEFAULT_TIMEOUT_MS)
  assert.ok(askedBudget > 12000, 'the budget must cover the requested wait: ' + askedBudget)
  // The worst case must stay under the harness budget that aborts tool calls.
  assert.ok(hugeBudget <= 25000, 'the budget must stay under the 25 s harness abort: ' + hugeBudget)
  assert.ok(hugeBudget >= WAIT_FOR_MAX_TIMEOUT_MS, 'the ceiling must be fully reachable')

  // Other methods keep the flat budget.
  assert.equal(sidecar.timeoutFor('call', { name: 'click' }), 10000)
})

test('P2-CONTRACT the JS wait tool and the helper method describe the same contract', async () => {
  const src = root => fs.readFileSync(path.join(pluginRoot, 'src', root), 'utf8')
  const helper = fs.readFileSync(
    path.join(pluginRoot, 'helper-linux', 'src', 'x11', 'waitfor.rs'),
    'utf8',
  )

  // The names must match across the wire, or every call would be an unknown method.
  assert.ok(helper.includes('pub const WAIT_FOR_TOOL: &str = "' + WAIT_FOR_METHOD + '"'))
  // The ceilings must be one number, not two that can drift. Read them out of the Rust
  // source and compare numerically: `20_000` and `20000` are the same ceiling, while a
  // genuinely different value still fails.
  const rustNumber = name => {
    const match = new RegExp('pub const ' + name + ': u64 = ([0-9_]+)').exec(helper)
    assert.ok(match, 'the helper must declare ' + name)
    return Number(match[1].replace(/_/g, ''))
  }
  assert.equal(rustNumber('MAX_TIMEOUT_MS'), WAIT_FOR_MAX_TIMEOUT_MS)
  assert.equal(rustNumber('DEFAULT_TIMEOUT_MS'), WAIT_FOR_DEFAULT_TIMEOUT_MS)

  // `wait_for` must not be smuggled into the official table.
  const window2 = fs.readFileSync(path.join(pluginRoot, 'helper-linux', 'src', 'x11', 'window2.rs'), 'utf8')
  const table = window2.slice(window2.indexOf('pub const WINDOW2_TOOLS'), window2.indexOf('];', window2.indexOf('pub const WINDOW2_TOOLS')))
  assert.equal(/wait_for/.test(table), false, 'wait_for must not be in WINDOW2_TOOLS')

  // The tool name carries the DSH extension prefix, like the other two extensions.
  assert.equal(WAIT_FOR_TOOL_NAME, 'computer_use_wait_for')
  assert.ok(src('tool.js').includes('WAIT_FOR_TOOL_NAME'))
})

test('P2-TOOL-REGISTRATION the wait tool is registered on Linux window2 and absent on Windows', async () => {
  const register = async config => {
    const registered = new Map()
    const dshComputerUse = {
      config,
      tools: async () => ({
        tools: EXPECTED_WINDOW2_TOOLS.map(name => ({
          name,
          description: name,
          parameters: { type: 'object', properties: {} },
        })),
      }),
      call: async (name, args) => ({ ok: true, name, value: { done: name }, images: [] }),
      health: async () => ({ ok: true }),
      releaseOverlay: async () => ({ ok: true }),
      shutdownSidecar: async () => {},
      experience: undefined,
    }
    const ctx = {
      dshComputerUse,
      on() {},
      effect(fn) { fn() },
      get() { return undefined },
      tools: { register(tool) { registered.set(tool.name, tool) } },
      systemPrompt: { section() {} },
    }
    await applyTools(ctx, ToolConfig({}))
    return registered
  }

  // Linux window2 (the backend this helper serves) exposes it.
  const linux = await register({ backend: 'linux', surface: 'computer' })
  assert.ok(linux.has(WAIT_FOR_TOOL_NAME), 'Linux window2 must expose ' + WAIT_FOR_TOOL_NAME)
  assert.ok(linux.has('computer_use_health'))

  // The Windows helper has no such method, so advertising it there would name a call that
  // always fails.
  const windows = await register({ backend: 'windows', surface: 'computer' })
  assert.equal(windows.has(WAIT_FOR_TOOL_NAME), false, 'Windows must not expose a Linux-only method')

  // The schema must describe the three conditions and both knobs. `defineTool` compiles the
  // implicit property map into a model-facing JSON Schema, so the result is the standard
  // object shape: the properties live under `properties`.
  const tool = linux.get(WAIT_FOR_TOOL_NAME)
  assert.equal(tool.parameters.type, 'object')
  for (const key of ['window', 'text_substring', 'element_name', 'gone', 'timeout_ms', 'poll_ms']) {
    assert.ok(tool.parameters.properties[key], 'the wait tool must document ' + key)
  }
  // `defineTool` compiles the implicit property map down to `{ type, properties }` and carries
  // neither `required` nor a top-level `additionalProperties`, so the required-window contract
  // lives in `execute` instead -- see the dispatch test, which asserts that a call without a
  // window never reaches the helper.
  assert.deepEqual(Object.keys(tool.parameters).sort(), ['properties', 'type'])
})

test('P2-DISPATCH the wait tool sends the helper method and the exact arguments', async () => {
  const calls = []
  const registered = new Map()
  const dshComputerUse = {
    config: { backend: 'linux', surface: 'computer' },
    tools: async () => ({
      tools: EXPECTED_WINDOW2_TOOLS.map(name => ({
        name,
        description: name,
        parameters: { type: 'object', properties: {} },
      })),
    }),
    currentTurnIdFor: () => 'turn-7',
    call: async (name, args, signal, meta) => {
      calls.push({ name, args, meta })
      return { ok: true, value: { matched: true, elapsedMs: 12, polls: 1 }, images: [] }
    },
    health: async () => ({ ok: true }),
    releaseOverlay: async () => ({ ok: true }),
    shutdownSidecar: async () => {},
  }
  const ctx = {
    dshComputerUse,
    on() {},
    effect(fn) { fn() },
    get() { return undefined },
    tools: { register(tool) { registered.set(tool.name, tool) } },
    systemPrompt: { section() {} },
  }
  await applyTools(ctx, ToolConfig({}))
  const tool = registered.get(WAIT_FOR_TOOL_NAME)

  const args = { window: { id: 42, app: 'gedit' }, text_substring: 'Ready', timeout_ms: 8000 }
  const result = await tool.execute(args, { agent: { id: 's' } })

  // The helper method, not the tool name: the prefix exists only on the model-facing side.
  assert.equal(calls.length, 1)
  assert.equal(calls[0].name, WAIT_FOR_METHOD)
  assert.deepEqual(calls[0].args, args)
  assert.equal(calls[0].meta.turnId, 'turn-7', 'the turn id must be the session turn, not the call')
  assert.equal(result.matched, true)

  // Exactly one condition is enforced before the helper is bothered.
  calls.length = 0
  await assert.rejects(
    () => tool.execute({ window: { id: 42 } }, { agent: { id: 's' } }),
    /exactly one of text_substring, element_name or gone/,
  )
  await assert.rejects(
    () => tool.execute({ window: { id: 42 }, text_substring: 'a', gone: 'b' }, { agent: { id: 's' } }),
    /exactly one of text_substring, element_name or gone/,
  )
  assert.equal(calls.length, 0, 'a malformed call must not reach the helper')

  // A missing window is refused too.
  await assert.rejects(
    () => tool.execute({ text_substring: 'Ready' }, { agent: { id: 's' } }),
    /needs a window object/,
  )
  assert.equal(calls.length, 0)
})

test('P2-TOOL-REGISTRATION backend=linux with surface=computer registers 13 window2 tools', async () => {
  const registered = new Map()
  const calls = []

  const dshComputerUse = {
    config: { backend: 'linux', surface: 'computer' },
    tools: async surface => {
      assert.equal(surface, 'computer')
      return {
        surface: 'computer',
        tools: EXPECTED_WINDOW2_TOOLS.map(name => ({
          name,
          description: 'Window2 Tool ' + name,
          parameters: { type: 'object', properties: {} },
        })),
      }
    },
    call: async (name, args, signal, meta) => {
      calls.push({ name, args, meta })
      return { ok: true, name, value: { done: name }, images: [] }
    },
    health: async () => ({ ok: true, codexRequired: false, backend: 'linux', surface: 'computer' }),
    releaseOverlay: async () => ({ ok: true }),
    shutdownSidecar: async () => {},
  }

  const ctx = {
    dshComputerUse,
    on() {},
    effect(fn) { fn() },
    get() { return undefined },
    tools: {
      register(tool) {
        registered.set(tool.name, tool)
      },
    },
    systemPrompt: { section() {} },
  }

  await applyTools(ctx, ToolConfig({}))

  for (const name of EXPECTED_WINDOW2_TOOLS) {
    assert.ok(registered.has(name), 'missing registered window2 tool: ' + name)
  }
  assert.ok(registered.has('computer_use_health'), 'must register health tool')
  assert.ok(registered.has('computer_use_experience'), 'must register experience tool')

  // Execute a window2 tool (drag) and check dispatch
  const dragTool = registered.get('drag')
  const res = await dragTool.execute(
    { window: { id: 101, app: 'gedit' }, from_x: 10, from_y: 20, to_x: 100, to_y: 200 },
    { agent: { id: 'test' } },
  )
  assert.deepEqual(res.value, { done: 'drag' })
  assert.equal(calls.length, 1)
  assert.equal(calls[0].name, 'drag')
  assert.deepEqual(calls[0].args, { window: { id: 101, app: 'gedit' }, from_x: 10, from_y: 20, to_x: 100, to_y: 200 })
})

test('P2-E2E-SMOKE-WINDOW2 sidecar end-to-end window2 surface with stub-linux-helper', async () => {
  const sidecar = new Sidecar({
    backend: 'linux',
    surface: 'computer',
    engineRoot: pluginRoot,
  })

  const origEnv = process.env.DSH_COMPUTER_USE_HELPER
  process.env.DSH_COMPUTER_USE_HELPER = stubHelper

  try {
    const health = await sidecar.request('health')
    assert.equal(health.backend, 'linux')
    assert.equal(health.codexRequired, false)

    const listed = await sidecar.request('tools')
    assert.equal(listed.surface, 'computer')
    const names = listed.tools.map(t => t.name)
    assert.deepEqual(names.sort(), [...EXPECTED_WINDOW2_TOOLS].sort())

    // Test list_windows
    const winList = await sidecar.request('call', { name: 'list_windows', arguments: {} })
    assert.equal(winList.ok, true)
    assert.ok(Array.isArray(winList.value))
    assert.equal(winList.value[0].id, 101)

    // Test get_window
    const win = await sidecar.request('call', { name: 'get_window', arguments: { id: 101 } })
    assert.equal(win.ok, true)
    assert.equal(win.value.id, 101)

    // Test get_window_state
    const state = await sidecar.request('call', {
      name: 'get_window_state',
      arguments: { window: { id: 101, app: 'org.gnome.TextEditor' } },
    })
    assert.equal(state.ok, true)
    assert.equal(state.images.length, 1)
    assert.ok(state.value.accessibility?.tree.includes('TextEditor'))
    assert.equal(state.value.screenshots.length, 1)

    // Test drag (which is rejected on linux 7-tool surface, but allowed on window2)
    const drag = await sidecar.request('call', {
      name: 'drag',
      arguments: {
        window: { id: 101, app: 'org.gnome.TextEditor' },
        from_x: 10,
        from_y: 20,
        to_x: 100,
        to_y: 200,
      },
    })
    assert.equal(drag.ok, true)
    assert.equal(drag.value.action, 'drag')

    // Test set_value
    const setValue = await sidecar.request('call', {
      name: 'set_value',
      arguments: {
        window: { id: 101, app: 'org.gnome.TextEditor' },
        element_index: 1,
        value: 'hello window2',
      },
    })
    assert.equal(setValue.ok, true)
    assert.equal(setValue.value.action, 'set_value')
    assert.equal(setValue.value.value, 'hello window2')

    // Test perform_secondary_action
    const sec = await sidecar.request('call', {
      name: 'perform_secondary_action',
      arguments: {
        window: { id: 101, app: 'org.gnome.TextEditor' },
        element_index: 1,
        action: 'Raise',
      },
    })
    assert.equal(sec.ok, true)
    assert.equal(sec.value.action, 'Raise')

    // Test activate_window
    const act = await sidecar.request('call', {
      name: 'activate_window',
      arguments: { window: { id: 101, app: 'org.gnome.TextEditor' } },
    })
    assert.equal(act.ok, true)
    assert.equal(act.value.action, 'activate_window')
  } finally {
    await sidecar.request('shutdown').catch(() => {})
    sidecar.dispose()
    if (origEnv !== undefined) process.env.DSH_COMPUTER_USE_HELPER = origEnv
    else delete process.env.DSH_COMPUTER_USE_HELPER
  }
})


// The five names both surfaces define. `drag`/`set_value`/... are window2-only and never
// collide, so only these five need a surface tag to resolve unambiguously.
const SHARED_CALL_NAMES = ['list_apps', 'click', 'press_key', 'type_text', 'scroll']
// The four of those that return an object carrying an explicit handler marker. `list_apps`
// answers with an array on both faces, so it is asserted through its shape instead.
const SHARED_OBJECT_CALLS = ['click', 'press_key', 'type_text', 'scroll']

test('P2-CALL-SURFACE callParamsFor tags only window2 turns and leaves P1 untouched', () => {
  const call = { name: 'click', arguments: { window: { id: 101 }, element_index: 1 } }

  // A window2 turn declares itself, so the helper can route the shared name natively.
  for (const surface of ['computer', 'windows', 'window2']) {
    assert.deepEqual(
      callParamsFor('call', call, { backend: 'linux', surface }),
      { ...call, surface: 'computer' },
      'surface=' + surface + ' must tag the call',
    )
  }

  // `all` is a union, not a face: on Linux it means window2 unless P1's linux was pinned,
  // which is exactly how listTools resolves it.
  assert.deepEqual(callParamsFor('call', call, { backend: 'linux', surface: 'all' }), { ...call, surface: 'computer' })
  assert.deepEqual(callParamsFor('call', {}, { backend: 'linux' }), {})

  // P1 callers keep the request shape they always sent: no new field at all.
  for (const config of [
    { backend: 'linux', surface: 'linux' },
    { backend: 'linux', surface: 'sky.window' },
    { backend: 'linux' },
    { backend: 'windows', surface: 'computer' },
    { backend: 'fake', surface: 'computer' },
  ]) {
    assert.deepEqual(callParamsFor('call', call, config), call, JSON.stringify(config) + ' must not tag')
  }

  // An explicit tag on the request is the caller's own declaration and wins.
  assert.deepEqual(
    callParamsFor('call', { ...call, surface: 'linux' }, { backend: 'linux', surface: 'computer' }),
    { ...call, surface: 'linux' },
  )

  // Only `call` is tagged: `tools` already carries its own surface and must not be rewritten.
  assert.deepEqual(callParamsFor('tools', { surface: 'all' }, { backend: 'linux', surface: 'computer' }), { surface: 'all' })
  assert.deepEqual(callParamsFor('health', {}, { backend: 'linux', surface: 'computer' }), {})
})

test('P2-CALL-SURFACE isWindow2Surface names the official face and its host spellings', () => {
  for (const name of ['computer', 'windows', 'window2', 'COMPUTER', 'Window2']) {
    assert.equal(isWindow2Surface(name), true, name + ' is the window2 face')
  }
  for (const name of ['linux', 'sky.window', 'all', 'desktop', 'browser', '', undefined, null]) {
    assert.equal(isWindow2Surface(name), false, String(name) + ' is not the window2 face')
  }
})

test('P2-CALL-SURFACE-DISPATCH a shared name reaches the window2 handler only when tagged', async () => {
  const sidecar = new Sidecar({ backend: 'linux', surface: 'computer', engineRoot: pluginRoot })
  const origEnv = process.env.DSH_COMPUTER_USE_HELPER
  process.env.DSH_COMPUTER_USE_HELPER = stubHelper

  try {
    await sidecar.request('tools')
    // window2 parameter shape: a window object plus an element index.
    const window2Click = await sidecar.request('call', {
      name: 'click',
      arguments: { window: { id: 101, app: 'org.gnome.TextEditor' }, element_index: 3 },
    })
    assert.equal(window2Click.ok, true)
    assert.equal(window2Click.value.handler, 'window2', 'a window2 turn must reach the window2 handler')
    assert.equal(window2Click.value.element_index, 3, 'the window2 shape must reach the handler intact')
    assert.equal(window2Click.value.window.id, 101, 'the window2 shape must reach the handler intact')

    for (const name of SHARED_OBJECT_CALLS) {
      const res = await sidecar.request('call', { name, arguments: { window: { id: 101 } } })
      assert.equal(res.ok, true, name + ' must answer')
      assert.equal(res.value.handler, 'window2', name + ' must reach the window2 handler on a window2 turn')
    }

    // list_apps is the fifth shared name: on window2 it groups windows under each app.
    const apps = await sidecar.request('call', { name: 'list_apps', arguments: {} })
    assert.equal(apps.ok, true)
    assert.ok(Array.isArray(apps.value) && Array.isArray(apps.value[0]?.windows), 'window2 list_apps must group windows under each app')
  } finally {
    await sidecar.request('shutdown').catch(() => {})
    sidecar.dispose()
    if (origEnv !== undefined) process.env.DSH_COMPUTER_USE_HELPER = origEnv
    else delete process.env.DSH_COMPUTER_USE_HELPER
  }
})

test('P2-CALL-SURFACE-P1-COMPAT an untagged P1 call keeps the sky.window handler', async () => {
  const sidecar = new Sidecar({ backend: 'linux', surface: 'linux', engineRoot: pluginRoot })
  const origEnv = process.env.DSH_COMPUTER_USE_HELPER
  process.env.DSH_COMPUTER_USE_HELPER = stubHelper

  try {
    await sidecar.request('tools')
    // The P1 parameter shape: an app id and absolute coordinates, no window object.
    const click = await sidecar.request('call', {
      name: 'click',
      arguments: { app: 'linux-window:101', x: 250, y: 350 },
    })
    assert.equal(click.ok, true)
    assert.equal(click.value.handler, 'sky.window', 'a P1 turn must keep the sky.window handler')
    assert.equal(click.value.x, 250)
    assert.equal(click.value.y, 350)

    for (const name of SHARED_OBJECT_CALLS) {
      const res = await sidecar.request('call', { name, arguments: { app: 'linux-window:101' } })
      assert.equal(res.ok, true, name + ' must answer')
      assert.equal(res.value.handler, 'sky.window', name + ' must keep the sky.window handler untagged')
    }

    // P1 list_apps answers with a flat catalog, not the window2 grouping.
    const apps = await sidecar.request('call', { name: 'list_apps', arguments: {} })
    assert.equal(apps.ok, true)
    assert.equal(apps.value[0].windows, undefined, 'P1 list_apps must not group windows')

    // A window2-only name has no P1 handler, so it goes to the native window2 dispatcher
    // even on a P1 face. It fails there for want of a window, not with an unsupported
    // name, which is what proves it reached a window2 handler at all.
    const drag = await sidecar.request('call', { name: 'drag', arguments: {} }).catch(err => ({ ok: false, error: err.message }))
    assert.equal(drag.ok, false, 'drag without a window must fail in the window2 handler')
    assert.match(
      String(drag.error || drag.value?.error || ''),
      /window is required/,
      'drag must be refused by the window2 handler, not as an unknown name',
    )
    assert.doesNotMatch(String(drag.error || drag.value?.error || ''), /unsupported method/, 'drag is not an unknown name')
  } finally {
    await sidecar.request('shutdown').catch(() => {})
    sidecar.dispose()
    if (origEnv !== undefined) process.env.DSH_COMPUTER_USE_HELPER = origEnv
    else delete process.env.DSH_COMPUTER_USE_HELPER
  }
})

test('P2-SIDECAR-ALL-SURFACE surface=all merges native window2 with python browser tools under backend=linux', async () => {
  const sidecarWindow2 = new Sidecar({
    backend: 'linux',
    surface: 'computer',
    engineRoot: pluginRoot,
  })

  let nativeRequestedSurface = null
  let pythonRequestedSurface = null

  sidecarWindow2.primary = {
    alive: true,
    rawRequest: async (method, payload) => {
      if (method === 'tools') {
        nativeRequestedSurface = payload.surface
        return {
          tools: EXPECTED_WINDOW2_TOOLS.map(name => ({ name })),
          surface: payload.surface,
        }
      }
      return { ok: true }
    },
  }

  sidecarWindow2.ensurePython = async () => ({
    rawRequest: async (method, payload) => {
      if (method === 'tools') {
        pythonRequestedSurface = payload.surface
        return {
          tools: [{ name: 'create_tab' }, { name: 'click' }], // click is duplicate, create_tab is extra
          surface: payload.surface,
        }
      }
      return { ok: true }
    },
  })

  const resAll = await sidecarWindow2.listTools({ surface: 'all' })
  assert.equal(nativeRequestedSurface, 'computer', 'native helper should receive surface=computer for window2 when surface=all')
  assert.equal(pythonRequestedSurface, 'all', 'python should receive surface=all')
  assert.equal(resAll.surface, 'all')
  const names = resAll.tools.map(t => t.name)
  for (const exp of EXPECTED_WINDOW2_TOOLS) {
    assert.ok(names.includes(exp), 'missing ' + exp)
  }
  assert.ok(names.includes('create_tab'), 'merged list must include create_tab from python')
  assert.equal(names.filter(n => n === 'click').length, 1, 'duplicate tools like click must be deduplicated')

  // Test P1 backward compatibility: when surface was configured as 'linux', surface='all' passes nativeSurface='linux'
  const sidecarP1 = new Sidecar({
    backend: 'linux',
    surface: 'linux',
    engineRoot: pluginRoot,
  })

  let p1NativeRequested = null
  sidecarP1.primary = {
    alive: true,
    rawRequest: async (method, payload) => {
      if (method === 'tools') {
        p1NativeRequested = payload.surface
        return {
          tools: EXPECTED_7_TOOLS.map(name => ({ name })),
          surface: payload.surface,
        }
      }
      return { ok: true }
    },
  }
  sidecarP1.ensurePython = async () => ({
    rawRequest: async () => ({ tools: [{ name: 'create_tab' }] }),
  })

  const resP1All = await sidecarP1.listTools({ surface: 'all' })
  assert.equal(p1NativeRequested, 'linux', 'native helper should receive surface=linux for P1 when surface=all')
  assert.ok(resP1All.tools.map(t => t.name).includes('create_tab'))
})



/**
 * IMG-EDGE: the maxImageEdge knob must reach the native helper through the
 * environment, because the official helper's hand-written argv parser aborts on
 * unknown flags (see spawnNative's own note). The assertion reads the *real*
 * environment of the spawned child by having the stub report it, so it cannot pass
 * on a value that was only computed and never handed to spawn().
 */
test('IMG-EDGE spawnNative exports DSH_COMPUTER_USE_MAX_IMAGE_EDGE only for a finite positive value', async () => {
  const original = process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE

  try {
    for (const [label, configured] of [
      ['a positive integer', 1280],
      ['a positive float', 900.5],
      ['zero (official, no cap)', 0],
      ['undefined (official default)', undefined],
      ['NaN', Number.NaN],
      ['Infinity', Number.POSITIVE_INFINITY],
      ['negative', -5],
    ]) {
      const sidecar = new Sidecar({
        backend: 'linux',
        engineRoot: pluginRoot,
        ...(configured === undefined ? {} : { maxImageEdge: configured }),
      })

      const slug = label.replace(/[^a-z0-9]+/gi, '-')
      const stub = path.join(os.tmpdir(), `img-edge-env-probe-${process.pid}-${slug}.mjs`)
      fs.writeFileSync(stub, `#!/usr/bin/env node
import readline from 'node:readline'
const env = {
  knob: process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE === undefined ? 'undefined' : process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE,
  argv: process.argv.slice(2),
}
const rl = readline.createInterface({ input: process.stdin, terminal: false })
rl.on('line', line => {
  const text = line.trim()
  if (!text) return
  let req
  try { req = JSON.parse(text) } catch { return }
  console.log(JSON.stringify({ id: req.id, ok: true, result: { ok: true, backend: 'linux', env } }))
})
`)

      const previous = process.env.DSH_COMPUTER_USE_HELPER
      process.env.DSH_COMPUTER_USE_HELPER = stub
      try {
        const health = await sidecar.request('health')
        const seen = (health && health.env) || (health && health.result && health.result.env)
        assert.ok(seen, 'the probe helper must report the environment it was launched with')
        const numeric = Number(configured)
        const expected = Number.isFinite(numeric) && numeric > 0 ? String(configured) : 'undefined'
        assert.equal(
          seen.knob,
          expected,
          `maxImageEdge configured as ${label} must export ${expected} to the native helper`,
        )
        // The official CLI surface must stay exactly --parent-pid + pid: no new argv.
        assert.deepEqual(seen.argv, ['--parent-pid', String(process.pid)], 'argv must stay the official surface')
      } finally {
        await sidecar.request('shutdown').catch(() => {})
        sidecar.dispose()
        if (previous !== undefined) process.env.DSH_COMPUTER_USE_HELPER = previous
        else delete process.env.DSH_COMPUTER_USE_HELPER
        fs.rmSync(stub, { force: true })
      }
    }
  } finally {
    if (original !== undefined) process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE = original
    else delete process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE
  }
})

test('IMG-EDGE the plugin never inherits a host DSH_COMPUTER_USE_MAX_IMAGE_EDGE', async () => {
  // The cap must come from config, never from whatever the host happens to have
  // exported: an inherited value would silently cap a helper the user never capped,
  // which is exactly the "official behaviour changed behind your back" failure D-E
  // exists to prevent.
  const previousHost = process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE
  process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE = '777'
  const stub = path.join(os.tmpdir(), `img-edge-inherit-probe-${process.pid}.mjs`)
  fs.writeFileSync(stub, `#!/usr/bin/env node
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin, terminal: false })
rl.on('line', line => {
  const text = line.trim()
  if (!text) return
  let req
  try { req = JSON.parse(text) } catch { return }
  const knob = process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE === undefined ? 'undefined' : process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE
  console.log(JSON.stringify({ id: req.id, ok: true, result: { ok: true, backend: 'linux', knob } }))
})
`)
  const previousHelper = process.env.DSH_COMPUTER_USE_HELPER
  process.env.DSH_COMPUTER_USE_HELPER = stub
  try {
    const sidecar = new Sidecar({ backend: 'linux', engineRoot: pluginRoot })
    const health = await sidecar.request('health')
    const knob = health.knob !== undefined ? health.knob : health.result && health.result.knob
    assert.equal(knob, 'undefined', 'an unconfigured plugin must not pass the host value to the helper')
    await sidecar.request('shutdown').catch(() => {})
    sidecar.dispose()
  } finally {
    if (previousHelper !== undefined) process.env.DSH_COMPUTER_USE_HELPER = previousHelper
    else delete process.env.DSH_COMPUTER_USE_HELPER
    if (previousHost !== undefined) process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE = previousHost
    else delete process.env.DSH_COMPUTER_USE_MAX_IMAGE_EDGE
    fs.rmSync(stub, { force: true })
  }
})

// ---------------------------------------------------------------------------
// P3: the browser-only Python channel on Linux.
//
// The Linux native helper serves the desktop faces and no browser catalog: the `tab_*`
// tools and the ExtensionHub bridge (127.0.0.1:8765) live in the Python engine, so a
// healthy native helper is only half the job. These cases pin the second channel, the
// spawn argv that opens it, the routing that feeds it, and -- just as important -- the
// paths that must NOT open it (Windows, fake, opt-out).
// ---------------------------------------------------------------------------

test('P3-ROUTING engineFor is the single routing decision point', () => {
  // `usesPython` must stay a thin wrapper so every existing caller keeps its semantics.
  assert.equal(usesPython('tools', { surface: 'browser' }), true)
  assert.equal(usesPython('tools', { surface: 'computer' }), false)
  assert.equal(usesPython('call', { name: 'create_tab' }, 'linux'), true)
  assert.equal(usesPython('call', { name: 'click' }, 'linux'), false)

  // Table-driven: the routing surface as a whole, not one name at a time.
  const table = [
    ['tools', { surface: 'computer' }, 'linux', 'native'],
    ['tools', { surface: 'linux' }, 'linux', 'native'],
    ['tools', { surface: 'browser' }, 'linux', 'python'],
    ['tools', { surface: 'all' }, 'linux', 'python'],
    ['tools', { surface: 'desktop' }, 'linux', 'native'],
    ['health', {}, 'linux', 'native'],
    ['prompt', {}, 'linux', 'native'],
    ['call', { name: 'create_tab' }, 'linux', 'python'],
    ['call', { name: 'tab_list' }, 'linux', 'python'],
    ['call', { name: 'browser_setup' }, 'linux', 'python'],
    // The two native faces own the shared names on Linux.
    ['call', { name: 'click' }, 'linux', 'native'],
    ['call', { name: 'list_apps' }, 'linux', 'native'],
    ['call', { name: 'drag' }, 'linux', 'native'],
    ['call', { name: 'list_windows' }, 'linux', 'native'],
    // Unknown names are Python's (the browser catalog is the open set).
    ['call', { name: 'tab_new' }, 'linux', 'python'],
    // batch_actions follows its members.
    ['call', { name: 'batch_actions', arguments: { actions: [{ name: 'click' }, { name: 'set_value' }] } }, 'linux', 'native'],
    ['call', { name: 'batch_actions', arguments: { actions: [{ name: 'tab_new' }] } }, 'linux', 'python'],
  ]
  for (const [method, params, backend, expected] of table) {
    assert.equal(
      engineFor(method, params, backend),
      expected,
      method + ' ' + JSON.stringify(params) + ' must route to ' + expected,
    )
    assert.equal(usesPython(method, params, backend), expected === 'python', 'usesPython must agree with engineFor')
  }
})

test('P3-PYTHON-SURFACE the engine is never spawned with a surface argparse rejects', () => {
  // computer_use/cli.py:18 accepts exactly these; `linux` is the native P1 face, not a
  // Python catalog, and passing it made argparse exit 2 -- the root cause of the dead
  // browser channel on Linux.
  assert.deepEqual([...PYTHON_SURFACES].sort(), ['all', 'browser', 'computer', 'desktop', 'gated', 'mac'])
  assert.equal(PYTHON_SURFACES.has('linux'), false)

  // Linux: the native faces become `browser`, the one catalog the native helper lacks.
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'computer' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'linux' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'sky.window' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'all' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'browser' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux', surface: 'mac' }), 'browser')
  assert.equal(pythonSurfaceFor({ backend: 'linux' }), 'browser')

  // On Linux every configured surface must resolve to one the engine's parser accepts:
  // that is the bug this function exists to fix.
  for (const surface of ['computer', 'linux', 'sky.window', 'all', 'browser', 'mac', 'desktop', 'gated', '']) {
    const resolved = pythonSurfaceFor({ backend: 'linux', surface })
    assert.equal(resolved, 'browser', 'linux surface=' + (surface || '(unset)') + ' must resolve to browser')
    assert.ok(PYTHON_SURFACES.has(resolved), 'and that value must be one argparse accepts')
  }

  // Windows/fake are the original primary paths: the configured value travels verbatim,
  // exactly as it did before this function existed -- including a value the engine would
  // refuse. Changing that is out of scope here and would alter working Windows behaviour.
  assert.equal(pythonSurfaceFor({ backend: 'windows', surface: 'computer' }), 'computer')
  assert.equal(pythonSurfaceFor({ backend: 'windows', surface: 'desktop' }), 'desktop')
  assert.equal(pythonSurfaceFor({ backend: 'windows', surface: 'all' }), 'all')
  assert.equal(pythonSurfaceFor({ backend: 'windows', surface: 'linux' }), 'linux', 'Windows passthrough is untouched')
  assert.equal(pythonSurfaceFor({ backend: 'windows' }), 'computer')
  assert.equal(pythonSurfaceFor({ backend: 'fake', surface: 'computer' }), 'computer')
  assert.equal(pythonSurfaceFor({ backend: 'helper', surface: 'gated' }), 'gated')
})

test('P3-CHANNEL-MODE auto opens the channel on Linux only, and never twice on Windows', () => {
  // Linux + a surface that needs browser tools -> eager.
  for (const surface of ['computer', 'browser', 'all']) {
    assert.equal(browserChannelMode({ backend: 'linux', surface }), 'eager', 'linux/' + surface + ' must open the channel')
  }
  // P1's native face keeps its lazy path.
  for (const surface of ['linux', 'sky.window', 'desktop', 'mac']) {
    assert.equal(browserChannelMode({ backend: 'linux', surface }), 'disabled', 'linux/' + surface + ' must stay lazy')
  }
  // Windows: the Python engine is already the (lazy) primary -- a second start is the bug
  // this whole guard exists for.
  for (const surface of ['computer', 'desktop', 'browser', 'all', '']) {
    assert.equal(browserChannelMode({ backend: 'windows', surface }), 'disabled', 'windows/' + surface + ' must not double-start')
  }
  assert.equal(browserChannelMode({ backend: 'live', surface: 'computer' }), 'disabled')
  assert.equal(browserChannelMode({ backend: 'helper', surface: 'computer' }), 'disabled')
  // fake runs Python as the primary, so there is no second channel to open.
  assert.equal(browserChannelMode({ backend: 'fake', surface: 'computer' }), 'disabled')

  // Explicit opt-out, and an explicit opt-in that still refuses the two unsafe cases.
  assert.equal(browserChannelMode({ backend: 'linux', surface: 'computer', browserChannel: 'off' }), 'disabled')
  assert.equal(browserChannelMode({ backend: 'linux', surface: 'linux', browserChannel: 'on' }), 'eager')
  assert.equal(browserChannelMode({ backend: 'windows', surface: 'computer', browserChannel: 'on' }), 'disabled')
  assert.equal(browserChannelMode({ backend: 'fake', surface: 'computer', browserChannel: 'on' }), 'disabled')
})

/** Run `body` with the stub helper + stub Python engine wired into the environment. */
async function withStubEngines(body) {
  const record = path.join(os.tmpdir(), 'dsh-stub-python-' + process.pid + '-' + Date.now() + '-' + Math.random().toString(36).slice(2) + '.jsonl')
  const saved = {
    helper: process.env.DSH_COMPUTER_USE_HELPER,
    python: process.env.PYTHON,
    record: process.env.DSH_STUB_PYTHON_RECORD,
    crash: process.env.DSH_STUB_PYTHON_CRASH,
  }
  process.env.DSH_COMPUTER_USE_HELPER = stubHelper
  process.env.PYTHON = stubPython
  process.env.DSH_STUB_PYTHON_RECORD = record
  delete process.env.DSH_STUB_PYTHON_CRASH
  const readSpawns = () =>
    fs.existsSync(record)
      ? fs.readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
      : []
  try {
    return await body({ record, readSpawns })
  } finally {
    for (const [key, name] of [
      ['helper', 'DSH_COMPUTER_USE_HELPER'],
      ['python', 'PYTHON'],
      ['record', 'DSH_STUB_PYTHON_RECORD'],
      ['crash', 'DSH_STUB_PYTHON_CRASH'],
    ]) {
      if (saved[key] !== undefined) process.env[name] = saved[key]
      else delete process.env[name]
    }
    try {
      fs.unlinkSync(record)
    } catch {
      // nothing recorded
    }
  }
}

test('P3-DUAL-CHANNEL start() opens the browser channel next to the native helper', async () => {
  await withStubEngines(async ({ readSpawns }) => {
    const sidecar = new Sidecar({ backend: 'linux', surface: 'computer', engineRoot: pluginRoot })
    try {
      await sidecar.start()

      // (1) The native helper is the desktop engine and stayed first.
      assert.equal(sidecar.primary.kind, 'native', 'the native helper must remain the desktop engine')

      // (2) The browser-only Python channel exists, is alive, and reports where it lives.
      assert.ok(sidecar.python.alive, 'the Python browser channel must be running after start()')
      assert.equal(sidecar.python.kind, 'python')
      assert.equal(sidecar.pythonChannel.state, 'started')
      assert.equal(sidecar.pythonChannel.surface, 'browser')
      assert.equal(
        sidecar.pythonChannel.endpoint,
        'http://127.0.0.1:8765',
        'the channel must report the ExtensionHub endpoint the extension polls',
      )
      assert.ok(sidecar.pythonChannel.pid > 0, 'the channel must report the engine pid')

      // (3) Evidence: the engine was really spawned, with an argv argparse accepts.
      const spawns = readSpawns()
      assert.ok(spawns.length >= 1, 'the Python engine must have been spawned')
      const argv = spawns[0].argv
      const surfaceIndex = argv.indexOf('--surface')
      assert.ok(surfaceIndex >= 0, 'the spawn must carry --surface')
      assert.equal(argv[surfaceIndex + 1], 'browser', 'the browser channel must be spawned as the browser surface')
      assert.deepEqual(argv.slice(-1), ['serve'], 'the spawn must end with the serve subcommand')
      const backendIndex = argv.indexOf('--backend')
      assert.equal(argv[backendIndex + 1], 'linux', 'the engine must be told the configured backend')

      // (4) Routing: browser calls reach the Python channel, desktop calls the native helper.
      const tabs = await sidecar.request('call', { name: 'tab_list', arguments: {} })
      assert.equal(tabs.ok, true)
      assert.equal(tabs.value.servedBy, 'python-stub', 'tab_list must be served by the Python engine')

      // window2 shape, because this sidecar is configured with surface=computer, which
      // callParamsFor tags as a window2 turn (P2 behavior, unchanged).
      const click = await sidecar.request('call', {
        name: 'click',
        arguments: { window: { id: 101, app: 'org.gnome.TextEditor' }, element_index: 2 },
      })
      assert.equal(click.ok, true, 'a desktop call must still answer')
      assert.equal(click.value?.handler, 'window2', 'the desktop call must reach the native window2 handler')
      assert.notEqual(click.value?.servedBy, 'python-stub', 'click must NOT be served by the Python engine')

      // (5) The browser catalog comes from Python; the desktop catalog from native.
      const browser = await sidecar.request('tools', { surface: 'browser' })
      const browserNames = browser.tools.map(tool => tool.name)
      assert.ok(browserNames.includes('create_tab'), 'the browser catalog must come from the Python engine')
      // The desktop catalog still comes from the native helper: this sidecar is configured
      // surface=computer, so it is the window2 13-method face, with no browser tool in it.
      const desktop = await sidecar.request('tools')
      assert.deepEqual(
        desktop.tools.map(tool => tool.name).sort(),
        [...EXPECTED_WINDOW2_TOOLS].sort(),
        'the desktop catalog must be served by the native helper',
      )

      // (6) shutdown reaches both engines, not just one.
      await sidecar.request('shutdown')
    } finally {
      sidecar.dispose()
    }
  })
})

test('P3-DUAL-CHANNEL-WINDOWS start() must not double-start the Python engine on Windows', async () => {
  await withStubEngines(async ({ readSpawns }) => {
    const sidecar = new Sidecar({ backend: 'windows', surface: 'computer', engineRoot: pluginRoot })
    try {
      await sidecar.start()
      assert.equal(sidecar.pythonChannel.state, 'disabled', 'Windows must not open a second Python channel')
      assert.equal(sidecar.python.alive, false, 'no Python child may be alive on a Windows start')
      assert.equal(readSpawns().length, 0, 'no Python engine may be spawned on a Windows start')
    } finally {
      sidecar.dispose()
    }
  })
})

test('P3-CHANNEL-FAILURE a Python that cannot start is reported, not fatal', async () => {
  await withStubEngines(async () => {
    // Every interpreter candidate must fail, or `ensurePython` correctly falls through to
    // the next one and the channel legitimately comes up -- which would test nothing. A
    // PATH shim shadowing `python` and `python3` makes all three candidates (PYTHON, the
    // stub, plus the two generic names) die the way a broken engine does: spawned, then
    // exiting before `health` is answered.
    const shim = fs.mkdtempSync(path.join(os.tmpdir(), 'dsh-broken-python-'))
    for (const name of ['python', 'python3']) {
      fs.writeFileSync(path.join(shim, name), '#!/bin/sh\nexit 1\n', { mode: 0o755 })
    }
    const savedPath = process.env.PATH
    process.env.PATH = shim + path.delimiter + (savedPath || '')
    // PYTHON is the first candidate `pythonCandidates` tries, so the (working) stub has to
    // be replaced too -- otherwise the channel legitimately comes up through it.
    process.env.PYTHON = path.join(shim, 'python')
    const sidecar = new Sidecar({
      backend: 'linux',
      surface: 'computer',
      engineRoot: pluginRoot,
      startupTimeoutMs: 3000,
    })
    try {
      // start() must still succeed: the desktop faces are healthy and the operator keeps
      // working; the broken browser channel is reported instead of taking the session down.
      await sidecar.start()
      assert.equal(sidecar.primary.kind, 'native', 'the native helper must still be the desktop engine')
      assert.equal(sidecar.pythonChannel.state, 'failed')
      assert.ok(sidecar.pythonChannel.error, 'a failure must carry its reason')
    } finally {
      sidecar.dispose()
      if (savedPath !== undefined) process.env.PATH = savedPath
      else delete process.env.PATH
      fs.rmSync(shim, { recursive: true, force: true })
    }
  })
})

test('P3-CHANNEL-OFF browserChannel=off restores the previous single-engine behaviour', async () => {
  await withStubEngines(async ({ readSpawns }) => {
    const sidecar = new Sidecar({
      backend: 'linux',
      surface: 'computer',
      engineRoot: pluginRoot,
      browserChannel: 'off',
    })
    try {
      await sidecar.start()
      assert.equal(sidecar.pythonChannel.state, 'disabled')
      assert.equal(readSpawns().length, 0, 'browserChannel=off must not spawn the engine')
      // The lazy path is untouched: a browser call still brings the engine up on demand.
      const browser = await sidecar.request('tools', { surface: 'browser' })
      assert.ok(browser.tools.map(tool => tool.name).includes('create_tab'))
      assert.equal(readSpawns().length, 1, 'the lazy ensurePython path must still work')
    } finally {
      sidecar.dispose()
    }
  })
})

test('P3-FAKE-PRIMARY a fake backend keeps Python as the primary and opens no second channel', async () => {
  await withStubEngines(async ({ readSpawns }) => {
    const sidecar = new Sidecar({ backend: 'fake', surface: 'computer', engineRoot: pluginRoot })
    try {
      await sidecar.start()
      assert.equal(sidecar.primary.kind, 'python', 'fake must run Python as its primary')
      assert.equal(sidecar.pythonChannel.state, 'primary')
      assert.equal(readSpawns().length, 1, 'fake must spawn the engine exactly once, never twice')
    } finally {
      sidecar.dispose()
    }
  })
})
