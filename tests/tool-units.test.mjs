/**
 * Unit tests for the 2026-10 hardening pass (P0-1 capture degradation, P1-3 nested
 * screenshot ids, P1-4 batch re-grounding, P1-5 lease recovery, P3-8 error text).
 *
 * These exercise the pure/async helpers in `src/tool.js` with a fake sidecar, so they
 * run offline (no helper, no desktop, no DSH host).
 *
 * Run: node --import ./tests/resolve-hooks.mjs --test tests/tool-units.test.mjs
 */
import assert from 'node:assert/strict'
import test from 'node:test'

import {
  captureGuidance,
  frameObservation,
  friendlyActionError,
  parseJsonLine,
  parsePageIndicator,
  recallFrame,
  rememberFrame,
  runBatchActions,
  runScrollTo,
  summarizeTree,
  translateNestedActionIds,
  translateScreenshotId,
} from '../src/tool.js'

const USER_INPUT = 'user input was detected in this window; call get_window_state before continuing'

/** A sidecar stub that records every call and answers from a queue of handlers. */
function fakeSidecar(handlers = {}) {
  const calls = []
  const call = async (name, args) => {
    calls.push({ name, args: args && typeof args === 'object' ? JSON.parse(JSON.stringify(args)) : args })
    const handler = handlers[name]
    const answer = typeof handler === 'function' ? handler(args, calls) : handler
    return answer === undefined ? { ok: true, value: {} } : answer
  }
  return { ctx: { dshComputerUse: { call } }, calls }
}

const state = () => ({ shotMap: new Map() })

// ---------------------------------------------------------------------------
// P1-3: nested screenshot ids
// ---------------------------------------------------------------------------

test('P1-3 translateNestedActionIds rewrites shot-N inside actions[] and fields[]', () => {
  const st = state()
  st.shotMap.set('shot-3', 'screenshot-0')
  const input = {
    window: { app: 'a', id: 1 },
    actions: [
      { name: 'click', arguments: { x: 10, y: 20, screenshotId: 'shot-3' } },
      { name: 'scroll', args: { screenshotId: 'shot-3', scrollY: 100 } },
      { name: 'click', arguments: { element_index: 2 } },
    ],
    fields: [{ element_index: 1, screenshotId: 'shot-3', value: 'x' }],
  }
  translateNestedActionIds(st, input)
  assert.equal(input.actions[0].arguments.screenshotId, 'screenshot-0')
  assert.equal(input.actions[1].args.screenshotId, 'screenshot-0')
  assert.equal(input.fields[0].screenshotId, 'screenshot-0')
  // An unknown id is left untouched (the helper reports it, recovery handles it).
  assert.equal(input.actions[2].arguments.screenshotId, undefined)
})

test('P1-3 top-level translation still works and ignores non-strings', () => {
  const st = state()
  st.shotMap.set('shot-1', 'screenshot-1')
  const input = { screenshotId: 'shot-1' }
  translateScreenshotId(st, input)
  assert.equal(input.screenshotId, 'screenshot-1')
  const other = { screenshotId: 42 }
  translateScreenshotId(st, other)
  assert.equal(other.screenshotId, 42)
})

// ---------------------------------------------------------------------------
// P1-4: batches run step by step, with a re-observation between coordinates
// ---------------------------------------------------------------------------

test('P1-4 consecutive coordinate steps get a lightweight observation in between', async () => {
  const { ctx, calls } = fakeSidecar({ click: { ok: true, value: {} } })
  const input = {
    window: { app: 'WeChatAppEx', id: 99 },
    actions: [
      { name: 'click', arguments: { x: 1, y: 2 } },
      { name: 'click', arguments: { x: 3, y: 4 } },
    ],
  }
  const out = await runBatchActions(ctx, state(), {}, {}, input)
  assert.equal(out.ok, true)
  assert.equal(out.value.executed, 2)
  const order = calls.map(c => c.name)
  assert.deepEqual(order, ['click', 'get_window_state', 'click'])
  // The interleaved observation must be screenshot-only: no UIA dump cost.
  const observe = calls[1]
  assert.equal(observe.args.include_screenshot, true)
  assert.equal(observe.args.include_text, false)
  // The shared window is threaded into steps that omit it.
  assert.deepEqual(calls[0].args.window, { app: 'WeChatAppEx', id: 99 })
})

test('P1-4 a non-coordinate step does not force an extra observation', async () => {
  const { ctx, calls } = fakeSidecar({ type_text: { ok: true }, click: { ok: true } })
  const out = await runBatchActions(ctx, state(), {}, {}, {
    actions: [
      { name: 'type_text', arguments: { text: 'hi' } },
      { name: 'click', arguments: { x: 1, y: 1 } },
    ],
  })
  assert.equal(out.ok, true)
  assert.deepEqual(calls.map(c => c.name), ['type_text', 'click'])
})

test('P1-4 a dirty refusal is re-observed and retried once, then reported as recovered', async () => {
  let firstClick = true
  const { ctx, calls } = fakeSidecar({
    click: () => {
      if (firstClick) { firstClick = false; return { ok: false, error: USER_INPUT } }
      return { ok: true, value: {} }
    },
  })
  const out = await runBatchActions(ctx, state(), {}, {}, {
    actions: [{ name: 'click', arguments: { x: 5, y: 6 } }],
  })
  assert.equal(out.ok, true)
  assert.equal(out.results[0].retried, true)
  assert.equal(out.results[0].ok, true)
  assert.deepEqual(calls.map(c => c.name), ['click', 'get_window_state', 'click'])
})

test('P1-4 an unrecoverable step stops the batch and names it', async () => {
  const { ctx } = fakeSidecar({ type_text: { ok: false, error: 'text is required' } })
  const out = await runBatchActions(ctx, state(), {}, {}, {
    actions: [
      { name: 'type_text', arguments: {} },
      { name: 'click', arguments: { x: 1, y: 1 } },
    ],
  })
  assert.equal(out.ok, false)
  assert.equal(out.failedAt, 1)
  assert.match(out.error, /step 1 \(type_text\) failed: text is required/)
  assert.equal(out.results.length, 1)
})

test('P1-4 an empty or nameless batch is rejected with a usable message', async () => {
  const { ctx } = fakeSidecar()
  const empty = await runBatchActions(ctx, state(), {}, {}, { actions: [] })
  assert.equal(empty.ok, false)
  assert.match(empty.error, /non-empty actions\[\]/)
  const nameless = await runBatchActions(ctx, state(), {}, {}, { actions: [{ arguments: {} }] })
  assert.equal(nameless.ok, false)
  assert.match(nameless.error, /step 1 has no name/)
})

// ---------------------------------------------------------------------------
// P0-1: degraded capture shapes and the stale-frame cache
// ---------------------------------------------------------------------------

test('P0-1 frameObservation produces both the image and a screenshot entry', () => {
  const frame = { data: 'AAAA', width: 800, height: 600 }
  const obs = frameObservation(frame, { source: 'gdi-printwindow' })
  assert.equal(obs.ok, true)
  assert.equal(obs.value.accessibility, null)
  const shot = obs.value.screenshots[0]
  assert.equal(shot.id, 'screenshot-0')
  assert.equal(shot.width, 800)
  assert.equal(shot.originX, 0)
  assert.equal(shot.source, 'gdi-printwindow')
  assert.equal(obs.images[0].mimeType, 'image/png')
  assert.equal(obs.images[0].data, 'AAAA')
})

test('P0-1 rememberFrame/recallFrame keep one frame per window and bound the cache', () => {
  const st = {}
  rememberFrame(st, 1, { data: 'a', width: 10, height: 10 })
  assert.equal(recallFrame(st, 1).data, 'a')
  assert.equal(recallFrame(st, 2), null)
  // Bad window ids are ignored entirely.
  rememberFrame(st, 0, { data: 'x' })
  assert.equal(st.frameCache.size, 1)
  for (let id = 2; id <= 9; id += 1) rememberFrame(st, id, { data: `f${id}`, width: 1, height: 1 })
  assert.ok(st.frameCache.size <= 6, `cache grew to ${st.frameCache.size}`)
  // The newest frames survive, the oldest is dropped first.
  assert.equal(recallFrame(st, 9).data, 'f9')
})

test('P0-1 captureGuidance names the recovery path and the oversize window', () => {
  assert.match(captureGuidance({ path: 'wgc-after-nudge' }), /repaint nudge and then succeeded/)
  const gdi = captureGuidance({ path: 'gdi-printwindow', geometry: { oversize: false, window: { width: 700, height: 500 }, screen: { width: 1652, height: 768 } } })
  assert.match(gdi, /GDI PrintWindow fallback/)
  assert.match(gdi, /window 700x500/)
  const stale = captureGuidance({ path: 'stale-frame', ageMs: 42_000 })
  assert.match(stale, /42s old, marked stale/)
  const dead = captureGuidance({ path: null, geometry: { oversize: true, screen: { width: 1652, height: 768 } } })
  assert.match(dead, /LARGER THAN THE SCREEN/)
  assert.match(dead, /element names and keyboard shortcuts still work/)
})

// ---------------------------------------------------------------------------
// P3-8: error text is one sentence plus a next step
// ---------------------------------------------------------------------------

test('P3-8 a huge helper dump is truncated with a pointer to list_windows', () => {
  const huge = `window id 7 not found. windows: ${'{"id":1,"title":"x"} '.repeat(80)}`
  const err = friendlyActionError({}, {}, huge)
  assert.ok(err, 'expected a rewritten error')
  assert.match(err.message, /truncated — call list_windows/)
  assert.ok(err.message.length < 1000)
})

test('P3-8 a message that already carries guidance is passed through untouched', () => {
  assert.equal(friendlyActionError({}, {}, 'window is gone; call list_windows to re-locate it'), null)
})

test('P3-8 the input-monitor message says the action did not run', () => {
  const err = friendlyActionError({}, {}, USER_INPUT)
  assert.match(err.message, /action did NOT run/)
  assert.match(err.message, /batch_actions re-observes/)
})

test('P3-8 a closed window clears the cached observation', () => {
  const st = { lastElements: [{ name: 'x' }], lastWindow: { app: 'a', id: 1 } }
  const err = friendlyActionError(st, {}, 'window id 8848414 was not found')
  assert.match(err.message, /CLOSED or gone/)
  assert.deepEqual(st.lastElements, [])
  assert.equal(st.lastWindow, null)
})

test('P3-8 an unrecognised short message is left to the caller', () => {
  assert.equal(friendlyActionError({}, {}, 'click requires x'), null)
})

test('P0-1 captureGuidance names the covered-window recovery too', () => {
  const covered = captureGuidance({ path: 'wgc-after-activate' })
  assert.match(covered, /covering the target/)
  assert.match(covered, /Call activate_window before observing/)
})

// ---------------------------------------------------------------------------
// Script stdout: a failing helper script still printed its verdict
// ---------------------------------------------------------------------------

test('parseJsonLine reads the verdict a failing helper script printed before exiting', () => {
  // Verified live: `uia-act.ps1` prints this and exits 2, so execFileSync throws and
  // the payload only survives on error.stdout.
  const stdout = 'PS noise\n{"ok":false,"error":"no element matching 这个控件不存在xyz"}\n'
  const parsed = parseJsonLine(stdout)
  assert.equal(parsed.ok, false)
  assert.match(parsed.error, /no element matching/)
})

test('parseJsonLine tolerates non-JSON and empty output', () => {
  assert.equal(parseJsonLine(''), null)
  assert.equal(parseJsonLine(undefined), null)
  assert.equal(parseJsonLine('just text\nno braces'), null)
  assert.equal(parseJsonLine('{"broken":'), null)
  assert.deepEqual(parseJsonLine('{"ok":true,"width":782}'), { ok: true, width: 782 })
})

// ---------------------------------------------------------------------------
// P1-4/P1-5: a wheel refused as "user input" is un-wedged and retried
// ---------------------------------------------------------------------------

test('runScrollTo clears the input monitor and retries a refused wheel', async () => {
  let first = true
  const { ctx, calls } = fakeSidecar({
    scroll: () => {
      if (first) { first = false; throw new Error(USER_INPUT) }
      return { ok: true }
    },
    clear_user_input: { cleared: true },
    get_window_state: { ok: true, value: { accessibility: null, screenshots: [{ id: 'screenshot-0', width: 784, height: 1628 }] } },
  })
  const out = await runScrollTo(ctx, { shotMap: new Map() }, { window: { app: 'a', id: 1 }, steps: 2, delta: 1000 }, {}, 12, () => null)
  assert.equal(out.stepsDone, 2)
  const order = calls.map(c => c.name)
  assert.deepEqual(order.slice(0, 3), ['scroll', 'clear_user_input', 'get_window_state'])
  assert.equal(order.filter(n => n === 'scroll').length, 3)
})

test('runScrollTo still self-heals stale viewport geometry', async () => {
  let first = true
  const { ctx } = fakeSidecar({
    scroll: () => {
      if (first) { first = false; throw new Error('point (392.0, 814.0) is outside viewport { originX: -8, originY: -8, width: 784, height: 1628 })') }
      return { ok: true }
    },
  })
  const out = await runScrollTo(ctx, { shotMap: new Map() }, { window: { app: 'a', id: 1 }, steps: 1 }, {}, 12, () => null)
  assert.equal(out.stepsDone, 1)
  assert.deepEqual(out.at, { x: 384, y: 806 })
})

// ---------------------------------------------------------------------------
// Wheel does not page a gallery: fall back to keys, driven by the page counter
// ---------------------------------------------------------------------------

test('parsePageIndicator reads 4/10 and 2/13 but ignores URL date runs', () => {
  assert.deepEqual(parsePageIndicator('Text 4/10 {x:380}'), { current: 4, total: 10 })
  assert.deepEqual(parsePageIndicator('ListItem 2/13'), { current: 2, total: 13 })
  assert.equal(parsePageIndicator('https://cos-cdn/doc/uploads/2026/01/12/x.jpg'), null)
  assert.equal(parsePageIndicator('img-1.jpg img-2.jpg img-10.jpg'), null)
  assert.equal(parsePageIndicator('0/10'), null)
  assert.equal(parsePageIndicator('99/10'), null)
  assert.equal(parsePageIndicator(''), null)
  // The widest plausible counter wins when the tree mentions several.
  assert.deepEqual(parsePageIndicator('Text 1/3\nText 4/10'), { current: 4, total: 10 })
})

test('a gallery that ignores the wheel is paged with keys until the last page', async () => {
  let page = 4
  const { ctx, calls } = fakeSidecar({
    scroll: { ok: true },
    press_key: () => { page = Math.min(page + 1, 10); return { ok: true } },
  })
  const out = await runScrollTo(
    ctx,
    { shotMap: new Map() },
    { window: { app: 'a', id: 7 }, steps: 3, delta: 4000 },
    {},
    12,
    () => ({ current: page, total: 10 }),
  )
  assert.equal(out.stepsDone, 3)
  assert.equal(out.page.from, 4)
  assert.equal(out.page.to, 10)
  assert.equal(out.page.total, 10)
  assert.equal(out.page.key, 'Page_Down')
  assert.equal(out.stepsDoneByKey, 6)
  assert.equal(calls.filter(c => c.name === 'press_key').length, 6)
})

test('a viewer that ignores both wheel and keys reports the stall instead of lying', async () => {
  const { ctx, calls } = fakeSidecar({ scroll: { ok: true }, press_key: { ok: true } })
  const out = await runScrollTo(
    ctx,
    { shotMap: new Map() },
    { window: { app: 'a', id: 7 }, steps: 2 },
    {},
    12,
    () => ({ current: 4, total: 10 }),
  )
  assert.equal(out.page.from, 4)
  assert.equal(out.page.to, 4)
  assert.equal(out.page.key, 'none')
  assert.equal(out.moved, 'none')
  assert.match(String(out.guidance), /computer_use_uia_act/)
  // It tried Page_Down, Right and Next, then gave up: 3 keys × (10-4+1) presses.
  assert.equal(calls.filter(c => c.name === 'press_key').length, 21)
  // …then it swept: one horizontal swipe per remaining page, and reported the stall.
  assert.equal(calls.filter(c => c.name === 'drag').length, 7)
})

test('a gallery that ignores wheel and keys is paged by swipe', async () => {
  // Measured live: moxueyuan's image preview moved 2/10 -> 3/10 only after a left drag.
  let page = 4
  const { ctx, calls } = fakeSidecar({
    scroll: { ok: true },
    press_key: { ok: true },
    drag: () => { page = Math.min(page + 1, 10); return { ok: true } },
  })
  const out = await runScrollTo(
    ctx,
    { shotMap: new Map() },
    { window: { app: 'a', id: 7 }, steps: 2 },
    {},
    12,
    () => ({ current: page, total: 10 }),
  )
  assert.equal(out.page.from, 4)
  assert.equal(out.page.to, 10)
  assert.equal(out.page.key, 'swipe')
  assert.equal(out.stepsDoneByKey, 6)
  // Three key strategies × 7 presses each found nothing; the six swipes did the work.
  assert.equal(calls.filter(c => c.name === 'press_key').length, 21)
  assert.equal(calls.filter(c => c.name === 'drag').length, 6)
})

test('summarizeTree keeps document text and interactive rows, drops container nesting', () => {
  const full = [
    '=== UIA fallback tree (.NET UIAutomation ControlView) ===',
    'These nodes have NO helper element_index. Act by x/y using the window-relative bounds.',
    '',
    '\tWindow 微信 {{x:0,y:0,w:784,h:1628}}',
    '\t\tPane 微信 {{x:8,y:8,w:768,h:1612}}',
    '\t\t\tPane 微信 {{x:8,y:8,w:768,h:1612}}',
    '\t\t\t\tText 医院全员安全生产知识考核 {{x:72,y:67,w:640,h:21}}',
    '\t\t\t\tText A、 {{x:144,y:216,w:24,h:16}}',
    '\t\t\t\tRadioButton A、无水乙醇 {{x:0,y:203,w:13,h:13}}',
    '\t\t\t\tButton 下一题 {{x:538,y:1573,w:101,h:35}}',
  ].join('\n')
  const out = summarizeTree(full)
  // Container panes carry no information of their own and are dropped.
  assert.ok(!out.includes('Pane 微信'), 'container panes should be dropped')
  // Document text and interactive nodes survive.
  assert.ok(out.includes('医院全员安全生产知识考核'), 'document text must survive')
  assert.ok(out.includes('RadioButton A、无水乙醇'), 'interactive node must survive')
  assert.ok(out.includes('Button 下一题'), 'button must survive')
  assert.ok(out.startsWith('=== UIA tree (summary'), 'summary header replaces the wall of instructions')
  // And it is meaningfully shorter than the input.
  assert.ok(out.length < full.length, 'summary must be smaller than the full tree')
})

test('summarizeTree is a no-op for empty input', () => {
  assert.equal(summarizeTree(''), '')
  // null/undefined normalize to the empty string rather than the literal "null".
  assert.equal(summarizeTree(null), '')
  assert.equal(summarizeTree(undefined), '')
})