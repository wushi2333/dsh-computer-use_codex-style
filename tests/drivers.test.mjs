/**
 * Unit tests for the driver facade — the pure/plumbing parts (path resolution, capability
 * gate). The actual exec paths (helper/script/uia) are exercised at runtime by tools.
 */
import assert from 'node:assert/strict'
import path from 'node:path'
import test from 'node:test'

import { scriptsDir, scriptPath, requireCapability } from '../src/drivers/index.js'

test('scriptsDir points at the plugin scripts/ folder', () => {
  const d = scriptsDir()
  assert.ok(path.isAbsolute(d), 'absolute')
  assert.ok(d.endsWith(`${path.sep}scripts`), `expected .../scripts, got ${d}`)
})

test('scriptPath resolves a bare name under scripts/ and passes an absolute path through', () => {
  const abs = scriptPath('uia-post.ps1')
  assert.equal(abs, path.join(scriptsDir(), 'uia-post.ps1'))
  const passthrough = scriptPath('C:\\some\\where\\x.ps1')
  assert.equal(passthrough, 'C:\\some\\where\\x.ps1')
})

test('requireCapability returns caps when ok and throws an actionable error when not', () => {
  const okState = { capabilities: { psHelper: { ok: true }, uia: { ok: true }, screenCap: { ok: true }, at: Date.now() } }
  const caps = requireCapability(okState, 'psHelper')
  assert.equal(caps.psHelper.ok, true)

  const badState = {
    capabilities: { psHelper: { ok: true }, uia: { ok: true }, screenCap: { ok: false, error: 'handle is invalid' }, at: Date.now() },
  }
  assert.throws(
    () => requireCapability(badState, 'screenCap', 'use the UIA tree'),
    (err) => /unavailable/.test(err.message) && /handle is invalid/.test(err.message) && /use the UIA tree/.test(err.message),
    'error names the missing capability, the reason, and the alternative',
  )
})
