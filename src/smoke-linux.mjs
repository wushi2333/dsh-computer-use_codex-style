import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Sidecar } from './sidecar.js'
import { nativeHelperCandidates } from './paths.js'

const engineRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const stubHelperPath = path.join(engineRoot, 'scripts', 'stub-linux-helper.mjs')

// If neither DSH_COMPUTER_USE_HELPER is set nor any native candidate exists, fallback to stub
const hasNative = nativeHelperCandidates(engineRoot).some(p => p && fs.existsSync(p))
if (!process.env.DSH_COMPUTER_USE_HELPER && !hasNative) {
  process.env.DSH_COMPUTER_USE_HELPER = stubHelperPath
}

const isStub = Boolean(
  process.env.DSH_COMPUTER_USE_HELPER &&
  (process.env.DSH_COMPUTER_USE_HELPER.includes('stub') ||
   process.env.DSH_COMPUTER_USE_HELPER.endsWith('.mjs') ||
   process.env.DSH_COMPUTER_USE_HELPER.endsWith('.js'))
)

const EXPECTED_LINUX_TOOLS = [
  'list_apps',
  'get_app_state',
  'screenshot',
  'click',
  'scroll',
  'press_key',
  'type_text',
]

const sidecar = new Sidecar({
  backend: 'linux',
  engineRoot,
})

try {
  // 1. Health check: ok:true and helper/surface identity correct
  const health = await sidecar.request('health')
  if (health.codexRequired) throw new Error('codex must not be required')
  if (!health.ok) throw new Error('health check failed')

  if (isStub) {
    if (health.backend !== 'linux') throw new Error(`expected backend=linux in stub, got ${health.backend}`)
  } else {
    // Real helper returns helper='dsh-computer-use', helperFlavor='linux-jsonl', surface='sky.window'
    if (health.helper !== 'dsh-computer-use' && health.backend !== 'linux') {
      throw new Error(`expected helper=dsh-computer-use or backend=linux, got helper=${health.helper}, backend=${health.backend}`)
    }
  }

  // 2. Tools list check: exactly 7 tools with expected names
  const listed = await sidecar.request('tools')
  const names = (listed.tools || []).map(t => t.name)
  for (const expected of EXPECTED_LINUX_TOOLS) {
    if (!names.includes(expected)) {
      throw new Error(`missing tool ${expected} in linux surface: ${names.join(', ')}`)
    }
  }
  if (names.length !== EXPECTED_LINUX_TOOLS.length) {
    throw new Error(`expected exactly 7 linux tools, got ${names.length}: ${names.join(', ')}`)
  }

  // 3. Out-of-bounds tool refusal: e.g. drag must be rejected
  const dragRejected = await sidecar.request('call', {
    name: 'drag',
    arguments: { from_x: 0, from_y: 0, to_x: 10, to_y: 10 },
  }).then(() => false).catch(() => true)

  if (!dragRejected) {
    throw new Error('expected out-of-bounds tool "drag" to be rejected on linux surface')
  }

  // 4. Verify tools can be called through sidecar
  // 4.1 list_apps
  const listAppsRes = await sidecar.request('call', { name: 'list_apps', arguments: {} })
  if (!listAppsRes.ok) throw new Error('list_apps call failed')
  if (isStub) {
    if (!Array.isArray(listAppsRes.value) || listAppsRes.value.length === 0) {
      throw new Error('list_apps did not return expected array in stub mode')
    }
  } else {
    if (!listAppsRes.value || typeof listAppsRes.value !== 'object') {
      throw new Error('list_apps did not return expected object in live mode')
    }
  }

  // 4.2 get_app_state (environment-dependent on Linux; log result without asserting ok)
  const appStateRes = await sidecar.request('call', {
    name: 'get_app_state',
    arguments: { app: 'org.gnome.TextEditor' },
  }).catch(err => ({ ok: false, error: err.message }))

  if (isStub) {
    if (!appStateRes.images?.length) throw new Error('expected get_app_state images[] in stub')
    if (typeof appStateRes.value?.text !== 'string') throw new Error('expected get_app_state text in value in stub')
    if (JSON.stringify(appStateRes.value).includes('data:image')) {
      throw new Error('value must not contain raw image data in stub')
    }
  }

  // 4.3 screenshot (must return images array with at least 1 image part and no raw base64 in value)
  const shotRes = await sidecar.request('call', {
    name: 'screenshot',
    arguments: { app: 'linux-window:101' },
  })
  if (!shotRes.ok) throw new Error('screenshot call failed')
  if (!shotRes.images?.length) throw new Error('expected screenshot images[] with at least 1 part')
  if (JSON.stringify(shotRes.value).includes('data:image')) {
    throw new Error('value must not contain raw image data')
  }

  // 4.4 click
  const clickRes = await sidecar.request('call', {
    name: 'click',
    arguments: { app: 'linux-window:101', x: 250, y: 350 },
  }).catch(err => ({ ok: false, error: err.message }))

  // 4.5 scroll (environment-dependent on Linux; log error/result without asserting ok)
  const scrollRes = await sidecar.request('call', {
    name: 'scroll',
    arguments: { app: 'linux-window:101', direction: 'down' },
  }).catch(err => ({ ok: false, error: err.message }))

  // 4.6 press_key
  const keyRes = await sidecar.request('call', {
    name: 'press_key',
    arguments: { app: 'linux-window:101', key: 'Return' },
  }).catch(err => ({ ok: false, error: err.message }))

  // 4.7 type_text
  const typeRes = await sidecar.request('call', {
    name: 'type_text',
    arguments: { app: 'linux-window:101', text: 'hello linux' },
  }).catch(err => ({ ok: false, error: err.message }))

  if (isStub) {
    if (!clickRes.value?.ok) throw new Error('click call failed in stub')
    if (!scrollRes.value?.ok) throw new Error('scroll call failed in stub')
    if (!keyRes.value?.ok) throw new Error('press_key call failed in stub')
    if (!typeRes.value?.ok) throw new Error('type_text call failed in stub')
  }

  console.log(JSON.stringify({
    ok: true,
    mode: isStub ? 'stub' : 'native',
    helper: health.helper || health.backend,
    surface: health.surface,
    tools: names.length,
    dragRejected: true,
    screenshotImages: shotRes.images.length,
    environmentNotes: {
      list_apps: listAppsRes.ok ? (listAppsRes.value?.apps ? `${listAppsRes.value.apps.length} apps` : 'ok') : listAppsRes.error,
      get_app_state: appStateRes.ok ? 'ok' : appStateRes.error,
      click: clickRes.ok ? (clickRes.value?.message || 'ok') : clickRes.error,
      scroll: scrollRes.ok ? 'ok' : scrollRes.error,
      press_key: keyRes.ok ? (keyRes.value?.message || 'ok') : keyRes.error,
      type_text: typeRes.ok ? (typeRes.value?.message || 'ok') : typeRes.error,
    },
  }))
} finally {
  await sidecar.request('shutdown').catch(() => {})
  sidecar.dispose()
}
