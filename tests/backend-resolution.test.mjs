import { test } from 'node:test'
import assert from 'node:assert/strict'
import { Context } from '@deepseek-ai/cordis'
import ComputerUseService, { Config as HostConfig, resolveBackend } from '../src/index.js'

test('resolveBackend resolves defaults by platform when rawBackend is unspecified or empty', () => {
  // linux -> 'linux'
  assert.equal(resolveBackend(undefined, 'linux'), 'linux')
  assert.equal(resolveBackend('', 'linux'), 'linux')
  assert.equal(resolveBackend(null, 'linux'), 'linux')

  // win32 -> 'windows'
  assert.equal(resolveBackend(undefined, 'win32'), 'windows')
  assert.equal(resolveBackend('', 'win32'), 'windows')
  assert.equal(resolveBackend(null, 'win32'), 'windows')

  // darwin -> 'windows' (conservative parity)
  assert.equal(resolveBackend(undefined, 'darwin'), 'windows')
  assert.equal(resolveBackend('', 'darwin'), 'windows')
  assert.equal(resolveBackend(null, 'darwin'), 'windows')

  // other platforms -> 'windows'
  assert.equal(resolveBackend(undefined, 'freebsd'), 'windows')
  assert.equal(resolveBackend('', 'openbsd'), 'windows')
  assert.equal(resolveBackend(null, 'sunos'), 'windows')
})

test('resolveBackend preserves explicit backend configuration on all platforms', () => {
  // Explicit config always takes precedence regardless of platform
  assert.equal(resolveBackend('fake', 'linux'), 'fake')
  assert.equal(resolveBackend('fake', 'win32'), 'fake')
  assert.equal(resolveBackend('fake', 'darwin'), 'fake')

  assert.equal(resolveBackend('windows', 'linux'), 'windows')
  assert.equal(resolveBackend('linux', 'win32'), 'linux')
  assert.equal(resolveBackend('live', 'linux'), 'live')
  assert.equal(resolveBackend('helper', 'win32'), 'helper')
})

test('Config schema validates allowed backend values and rejects invalid ones', () => {
  assert.equal(HostConfig({ backend: 'windows' }).backend, 'windows')
  assert.equal(HostConfig({ backend: 'linux' }).backend, 'linux')
  assert.equal(HostConfig({ backend: 'live' }).backend, 'live')
  assert.equal(HostConfig({ backend: 'fake' }).backend, 'fake')
  assert.equal(HostConfig({ backend: 'helper' }).backend, 'helper')

  assert.throws(() => HostConfig({ backend: 'darwin' }), /expected/)
  assert.throws(() => HostConfig({ backend: 'invalid' }), /expected/)
  assert.throws(() => HostConfig({ backend: '' }), /expected/)
})

test('ComputerUseService resolves backend and surface when backend is omitted or empty', async () => {
  // Direct instantiation with empty config under current platform (linux)
  const serviceDefault = new ComputerUseService(new Context(), {})
  assert.equal(serviceDefault.config.backend, 'linux')
  assert.equal(serviceDefault.config.surface, 'linux')

  const serviceEmpty = new ComputerUseService(new Context(), { backend: '' })
  assert.equal(serviceEmpty.config.backend, 'linux')
  assert.equal(serviceEmpty.config.surface, 'linux')

  const serviceUndef = new ComputerUseService(new Context(), { backend: undefined })
  assert.equal(serviceUndef.config.backend, 'linux')
  assert.equal(serviceUndef.config.surface, 'linux')

  // Explicit backend overrides default
  const serviceFake = new ComputerUseService(new Context(), { backend: 'fake' })
  assert.equal(serviceFake.config.backend, 'fake')
  assert.equal(serviceFake.config.surface, 'computer')

  const serviceWin = new ComputerUseService(new Context(), { backend: 'windows' })
  assert.equal(serviceWin.config.backend, 'windows')
  assert.equal(serviceWin.config.surface, 'computer')

  // Cordis plugin mounting with empty config
  const ctx = new Context()
  await ctx.plugin(ComputerUseService, {})
  assert.equal(ctx.dshComputerUse.config.backend, 'linux')

  // Health report exposes the resolved backend
  const health = await ctx.dshComputerUse.health()
  assert.equal(health.backend, 'linux')
})
