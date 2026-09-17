#!/usr/bin/env node
/**
 * Test double for the Python engine's stdio sidecar (computer_use/rpc.py `serve_stdio`).
 *
 * The real engine cannot be spawned in a unit test: it is a Python process whose browser
 * surface needs a running Chromium/CDP bridge. This stub speaks the same JSONL protocol --
 * `{id, method, params}` in, `{id, ok, result}` out -- and records the argv it was given,
 * so a test can prove *how* the sidecar spawned the browser-only channel (`--surface`,
 * `--backend`, `serve`) without a browser.
 *
 * The surface/argv are reported through `health`, which is exactly the call
 * `Sidecar.ensurePython` waits for before it declares the channel started.
 */
import fs from 'node:fs'
import readline from 'node:readline'

// Test switch: emulate an engine that dies on startup (e.g. a missing import). The sidecar
// must report the browser channel as failed and keep the desktop helper working.
if (process.env.DSH_STUB_PYTHON_CRASH === '1') {
  process.stderr.write('stub python engine: simulated startup failure\n')
  process.exit(1)
}

const argv = process.argv.slice(2)
const argOf = name => {
  const index = argv.indexOf(name)
  return index >= 0 && index + 1 < argv.length ? argv[index + 1] : null
}
const surface = argOf('--surface') || 'computer'
const backend = argOf('--backend') || 'windows'
const recordPath = process.env.DSH_STUB_PYTHON_RECORD || ''
if (recordPath) {
  try {
    fs.appendFileSync(recordPath, JSON.stringify({ argv, surface, backend, pid: process.pid }) + '\n')
  } catch {
    // The record is test evidence only; never fail the channel over it.
  }
}

const BROWSER_TOOLS = [
  { name: 'create_tab', description: 'Create a tab.', parameters: { type: 'object', properties: {} } },
  { name: 'tab_list', description: 'List tabs.', parameters: { type: 'object', properties: {} } },
  { name: 'tab_goto', description: 'Navigate a tab.', parameters: { type: 'object', properties: {} } },
  { name: 'tab_close', description: 'Close a tab.', parameters: { type: 'object', properties: {} } },
]

const rl = readline.createInterface({ input: process.stdin })
rl.on('line', line => {
  const text = String(line).trim()
  if (!text) return
  let request
  try {
    request = JSON.parse(text)
  } catch {
    return
  }
  const { id, method, params } = request
  const emit = result => process.stdout.write(JSON.stringify({ id, ok: true, result }) + '\n')
  if (method === 'health') {
    emit({ ok: true, codexRequired: false, backend, surface, argv })
    return
  }
  if (method === 'tools') {
    const asked = String(params?.surface || surface)
    if (asked === 'browser' || asked === 'all') {
      emit({ tools: BROWSER_TOOLS, surface: asked, disabledMemberIds: [] })
      return
    }
    emit({ tools: [], surface: asked, disabledMemberIds: [] })
    return
  }
  if (method === 'call') {
    emit({ ok: true, name: String(params?.name || ''), value: { servedBy: 'python-stub', surface }, images: [] })
    return
  }
  if (method === 'shutdown' || method === 'interrupt' || method === 'cancel') {
    emit({ ok: true })
    if (method === 'shutdown') setTimeout(() => process.exit(0), 10)
    return
  }
  emit({ ok: false, error: 'unknown method ' + method })
})
