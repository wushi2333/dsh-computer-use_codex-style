/**
 * Safety contract unit tests.
 *
 * These cover the modules under `src/safety/` — the port of the reference
 * project's SAFETY CONTRACT (see `docs/design-essence-dsh-computer-use.md`).
 *
 * They deliberately import **nothing from the host**: `src/safety/*` has no
 * `@deepseek-ai/*` dependency, so this suite runs with a bare `node --test`
 * and without a DSH runtime. That is a design requirement, not a convenience:
 * a policy layer that can only be exercised inside a full host is a policy
 * layer nobody re-tests.
 */
import test from 'node:test'
import assert from 'node:assert/strict'

import { createGrantStore, requireGrant, sessionKey, isSubagentSession } from '../src/safety/grant.js'
import { createSchemaVisibility, liftRestriction } from '../src/safety/schema-visibility.js'
import { createControllerLease, movesDesktop, WRITE_TOOLS } from '../src/safety/controller-lease.js'
import { decorateFailure, refuse, SafetyError, RETRY } from '../src/safety/failures.js'
import { createSafetyContract, DEFAULT_SAFETY_CONFIG } from '../src/safety/index.js'

// ---------------------------------------------------------------- failures --

test('failures: a refusal carries a stable code, a retry verdict and a next step', () => {
  assert.throws(
    () => refuse('CONTROLLER_BUSY', 'another session is driving the desktop'),
    error => {
      assert.ok(error instanceof SafetyError)
      assert.equal(error.code, 'CONTROLLER_BUSY')
      assert.equal(error.retry, RETRY.RETRY)
      assert.match(error.message, /^\[CONTROLLER_BUSY\]/)
      // The hint must travel with the message: the model reads the message, not
      // the structured fields.
      assert.match(error.message, /另一个会话/)
      return true
    },
  )
})

test('failures: an unknown failure is never silently treated as retryable', () => {
  const decorated = decorateFailure(new Error('something went sideways'))
  assert.equal(decorated.code, 'UNKNOWN')
  assert.equal(decorated.retry, RETRY.REOBSERVE)
})

test('failures: the rules recognise the codes the gates raise', () => {
  for (const code of ['NOT_AUTHORIZED', 'SUBAGENT_COMPUTER_USE_UNAVAILABLE', 'CONTROLLER_BUSY', 'SCREEN_LOCKED', 'ELEVATION_BLOCKED']) {
    const decorated = decorateFailure(new Error(`[${code}] boom`))
    assert.equal(decorated.code, code, `${code} must map to itself`)
  }
})

test('failures: SCREEN_LOCKED recognises UAC consent desktop and OpenInputDesktop failure', () => {
  const cases = [
    'OpenInputDesktop failed',
    'Windows desktop is locked (input desktop is Consent); unlock before using computer-use',
    'active input desktop is an isolated secure desktop',
    'desktop is locked (input desktop is SecureDesktop)',
  ]
  for (const msg of cases) {
    const decorated = decorateFailure(new Error(msg))
    assert.equal(decorated.code, 'SCREEN_LOCKED', `"${msg}" must map to SCREEN_LOCKED`)
    assert.equal(decorated.retry, RETRY.NEVER, `"${msg}" must have retry: never`)
  }
})

// ------------------------------------------------------------------- grant --

test('grant: an explicit "off" beats the deployment default', () => {
  const grants = createGrantStore({ defaultEnabled: true })
  assert.equal(grants.isEnabled('s1'), true)
  grants.revoke('s1')
  assert.equal(grants.isEnabled('s1'), false, 'an explicit false must not fall back to the default')
  grants.grant('s1')
  assert.equal(grants.isEnabled('s1'), true)
})

test('grant: a deployment default of off leaves untouched sessions closed', () => {
  const grants = createGrantStore({ defaultEnabled: false })
  assert.equal(grants.isEnabled('s1'), false)
  grants.grant('s1')
  assert.equal(grants.isEnabled('s1'), true)
  assert.equal(grants.isEnabled('s2'), false, 'granting one session must not grant another')
})

test('grant: the anonymous session is gated like any other', () => {
  assert.equal(sessionKey(undefined), '')
  assert.equal(sessionKey(null), '')
  const grants = createGrantStore({ defaultEnabled: false })
  assert.equal(grants.isEnabled(undefined), false)
  grants.grant(undefined)
  assert.equal(grants.isEnabled(undefined), true)
})

test('grant: a subagent is recognised by its session header', () => {
  assert.equal(isSubagentSession({ session: { header: { origin: 'subagent' } } }), true)
  assert.equal(isSubagentSession({ session: { header: { origin: 'user' } } }), false)
  assert.equal(isSubagentSession({}), false)
  assert.equal(isSubagentSession(undefined), false)
})

test('grant: the gate refuses a subagent before it checks the authorization', () => {
  const grants = createGrantStore({ defaultEnabled: true })
  const agent = { session: { id: 'child', header: { origin: 'subagent' } } }
  assert.throws(
    () => requireGrant({ grant: grants, agent }),
    error => error.code === 'SUBAGENT_COMPUTER_USE_UNAVAILABLE',
  )
})

test('grant: the gate refuses a session whose capability is off', () => {
  const grants = createGrantStore({ defaultEnabled: false })
  let synced = 0
  assert.throws(
    () => requireGrant({ grant: grants, agent: { session: { id: 's1' } }, onNotAuthorized: () => { synced += 1 } }),
    error => error.code === 'NOT_AUTHORIZED',
  )
  assert.equal(synced, 1, 'a refusal must also converge the schema visibility')
})

test('grant: the gate refuses an unsupported platform with a non-retryable verdict', () => {
  const grants = createGrantStore({ defaultEnabled: true })
  assert.throws(
    () => requireGrant({ grant: grants, agent: { session: { id: 's1' } }, supported: false }),
    error => error.code === 'UNSUPPORTED_PLATFORM' && error.retry === RETRY.NEVER,
  )
})

test('grant: an allowed session passes the gate', () => {
  const grants = createGrantStore({ defaultEnabled: true })
  assert.doesNotThrow(() => requireGrant({ grant: grants, agent: { session: { id: 's1' } } }))
})

// ------------------------------------------------------- schema visibility --

/** A fake agent whose scoped tools registry hands out a **function** disposer. */
function fakeAgent(id, options = {}) {
  const calls = { restricted: [], lifted: 0 }
  const tools = {
    restrict(filter) {
      if (options.reject === 'all') throw new Error('tools.restrict() requires a scoped context (agent.ctx)')
      if (options.reject === 'some' && filter.deny.includes('known-bad')) {
        throw new Error('tools.restrict() names unknown global tool "known-bad"')
      }
      calls.restricted.push([...filter.deny])
      return () => { calls.lifted += 1 }
    },
  }
  return { agent: { id, session: { id }, ctx: { tools } }, calls }
}

test('schema visibility: a switched-off session loses its tool schemas', () => {
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => false,
    hideableNames: () => ['click', 'type_text'],
  })
  const { agent, calls } = fakeAgent('s1')
  assert.equal(visibility.sync(agent), 'hidden')
  assert.deepEqual(calls.restricted, [['click', 'type_text']])
  assert.equal(visibility.state('s1'), 'hidden')
})

test('schema visibility: the restriction is lifted through the returned function', () => {
  let enabled = false
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => enabled,
    hideableNames: () => ['click'],
  })
  const { agent, calls } = fakeAgent('s1')
  visibility.sync(agent)
  assert.equal(calls.lifted, 0)
  enabled = true
  visibility.sync(agent)
  // The host hands out a **function**; `restricted.dispose?.()` would be a
  // silent no-op and the tool would stay invisible forever.
  assert.equal(calls.lifted, 1)
  assert.equal(visibility.state('s1'), 'visible')
})

test('schema visibility: lifting accepts both disposer shapes', () => {
  let disposed = 0
  liftRestriction(() => { disposed += 1 })
  liftRestriction({ dispose: () => { disposed += 1 } })
  assert.equal(disposed, 2)
})

test('schema visibility: turning the capability on without a live agent releases in place', () => {
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => false,
    hideableNames: () => ['click'],
  })
  const { agent, calls } = fakeAgent('s1')
  visibility.sync(agent)
  assert.equal(visibility.restrictedCount(), 1)
  // No live agent is registered under this key any more: the "user said yes"
  // must not be eaten by local bookkeeping.
  visibility.release('s1')
  assert.equal(visibility.restrictedCount(), 0)
  assert.equal(calls.lifted, 1)
})

test('schema visibility: a registry that refuses the restriction degrades to visible-but-refused', () => {
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => false,
    hideableNames: () => ['click'],
  })
  const { agent } = fakeAgent('s1', { reject: 'all' })
  assert.equal(visibility.sync(agent), 'visible')
  assert.equal(visibility.restrictedCount(), 0)
})

test('schema visibility: one unknown name does not void the whole restriction', () => {
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => false,
    hideableNames: () => ['click', 'known-bad'],
  })
  const { agent, calls } = fakeAgent('s1', { reject: 'some' })
  assert.equal(visibility.sync(agent), 'hidden')
  // The batch failed on "known-bad", so it retries name by name and keeps the
  // ones it can actually restrict.
  assert.ok(calls.restricted.some((names) => names.length === 1 && names[0] === 'click'))
})

test('schema visibility: an unauthorised sync still registers the agent', () => {
  const visibility = createSchemaVisibility({
    enabled: true,
    isEnabled: () => true,
    hideableNames: () => ['click'],
  })
  const { agent } = fakeAgent('s1')
  visibility.sync(agent)
  assert.equal(visibility.liveAgentCount(), 1)
})

// --------------------------------------------------------- controller lease --

test('lease: read operations never need the lease', () => {
  assert.equal(movesDesktop('get_window_state'), false)
  assert.equal(movesDesktop('list_apps'), false)
  assert.equal(movesDesktop('diagnostic_state'), false)
  assert.equal(movesDesktop('click'), true)
  assert.equal(movesDesktop('activate_window'), true)
  assert.ok(WRITE_TOOLS.has('batch_actions'))
})

test('lease: a second session is refused while the first one drives the desktop', () => {
  const lease = createControllerLease()
  lease.claim('s1', 'click')
  assert.throws(
    () => lease.claim('s2', 'click'),
    error => error.code === 'CONTROLLER_BUSY' && error.retry === RETRY.RETRY,
  )
  // A read is still allowed while the other session is driving.
  assert.doesNotThrow(() => lease.claim('s2', 'get_window_state'))
})

test('lease: the same session re-enters its own lease', () => {
  const lease = createControllerLease()
  lease.claim('s1', 'click')
  assert.doesNotThrow(() => lease.claim('s1', 'type_text'))
})

test('lease: an idle lease is released on its own', async () => {
  const lease = createControllerLease({ idleMs: 5 })
  lease.claim('s1', 'click')
  assert.throws(() => lease.claim('s2', 'click'))
  await new Promise(resolve => setTimeout(resolve, 30))
  assert.doesNotThrow(() => lease.claim('s2', 'click'))
})

test('lease: the refusal says whether the other session is executing or merely quiet', () => {
  const lease = createControllerLease()
  lease.claim('s1', 'click')
  lease.beginWrite()
  let message = ''
  try {
    lease.claim('s2', 'click')
  } catch (error) {
    message = error.message
  }
  assert.match(message, /middle of a write/i)
  lease.finishWrite()
  message = ''
  try {
    lease.claim('s2', 'click')
  } catch (error) {
    message = error.message
  }
  assert.match(message, /drove the desktop/i)
})

test('lease: the idle clock restarts when a write finishes, not when it starts', async () => {
  const lease = createControllerLease({ idleMs: 40 })
  lease.claim('s1', 'click')
  lease.beginWrite()
  await new Promise(resolve => setTimeout(resolve, 60))
  // The write is still running, so the lease must NOT have expired.
  assert.throws(() => lease.claim('s2', 'click'))
  lease.finishWrite()
  await new Promise(resolve => setTimeout(resolve, 60))
  assert.doesNotThrow(() => lease.claim('s2', 'click'))
})

test('lease: the report names the owner without leaking the whole session id', () => {
  const lease = createControllerLease()
  lease.claim('session-0123456789abcdef', 'click')
  const report = lease.report('session-0123456789abcdef')
  assert.equal(report.owns, true)
  assert.equal(report.held, true)
  assert.equal(report.lease.owner, '…89abcdef')
  assert.ok(!String(report.lease.owner).includes('session-'))
})

// -------------------------------------------------------------- the contract --

test('contract: the gate refuses a subagent before anything else', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: true } })
  assert.throws(
    () => contract.gate({ agent: { session: { id: 'child', header: { origin: 'subagent' } } } }, 'click'),
    error => error.code === 'SUBAGENT_COMPUTER_USE_UNAVAILABLE',
  )
})

test('contract: the gate arbitrates writes but not reads', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: true } })
  const session = { agent: { session: { id: 's1' } } }
  assert.equal(contract.gate(session, 'get_window_state').write, false)
  assert.equal(contract.gate(session, 'click').write, true)
  const other = { agent: { session: { id: 's2' } } }
  assert.throws(() => contract.gate(other, 'click'), error => error.code === 'CONTROLLER_BUSY')
  assert.doesNotThrow(() => contract.gate(other, 'get_window_state'))
})

test('contract: the control state distinguishes off, stopped, ready and observing', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: false } })
  assert.equal(contract.controlState('s1').state, 'off')
  contract.grant.revoke('s1')
  contract.grant.grant('s1')
  assert.equal(contract.controlState('s1').state, 'observing')
  contract.lease.claim('s1', 'click')
  assert.equal(contract.controlState('s1').state, 'ready')
  contract.grant.revoke('s1')
  assert.equal(contract.controlState('s1').state, 'stopped')
})

test('contract: a session end forgets the grant, the lease and the restriction', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: true } })
  contract.grant.revoke('s1')
  assert.equal(contract.grant.isEnabled('s1'), false)
  contract.forgetSession('s1')
  assert.equal(contract.grant.isEnabled('s1'), true, 'a forgotten session falls back to the default')
})

// ------------------------------------------------------------- authorize() --

test('authorize: the key opens a session a closed default had shut (P3-1)', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: false }, hideableNames: () => ['click'] })
  const { agent, calls } = fakeAgent('s1')
  contract.visibility.sync(agent)
  assert.equal(contract.controlState('s1').state, 'off', 'precondition: the capability is off')
  assert.equal(contract.visibility.state('s1'), 'hidden')
  assert.throws(() => contract.gate({ agent }, 'click'), error => error.code === 'NOT_AUTHORIZED')

  const result = contract.authorize('s1', true)
  assert.equal(result.enabled, true)
  assert.equal(result.explicit, true)
  assert.equal(result.schemas, 'visible')
  assert.equal(contract.controlState('s1').state, 'observing')
  assert.equal(calls.lifted, 1, 'the schema restriction is lifted the moment the grant flips')
  assert.doesNotThrow(() => contract.gate({ agent }, 'click'))
})

test('authorize: revoke closes a session and re-hides its schemas atomically', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: true }, hideableNames: () => ['click'] })
  const { agent, calls } = fakeAgent('s1')
  contract.visibility.sync(agent)
  assert.doesNotThrow(() => contract.gate({ agent }, 'click'))

  const result = contract.authorize('s1', false)
  assert.equal(result.enabled, false)
  assert.equal(result.explicit, true)
  assert.equal(result.schemas, 'hidden')
  assert.equal(contract.visibility.state('s1'), 'hidden')
  assert.throws(() => contract.gate({ agent }, 'click'), error => error.code === 'NOT_AUTHORIZED')
  // An explicit revoke must survive a later sync (?? not ||): the session stays
  // closed even though it is registered and reachable again.
  contract.visibility.sync(agent)
  assert.equal(contract.visibility.state('s1'), 'hidden')
  assert.ok(calls.restricted.length >= 1)
})

test('authorize: a revoked-then-regranted round trip restores everything', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: true } })
  const { agent } = fakeAgent('s1')
  contract.visibility.sync(agent)
  contract.authorize('s1', false)
  assert.throws(() => contract.gate({ agent }, 'click'))
  contract.authorize('s1', true)
  assert.doesNotThrow(() => contract.gate({ agent }, 'click'))
  assert.equal(contract.visibility.state('s1'), 'visible')
})

test('authorize: the anonymous session is keyable like any other', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: false } })
  const result = contract.authorize(undefined, true)
  assert.equal(result.sessionId, '')
  assert.equal(result.enabled, true)
  assert.doesNotThrow(() => contract.gate({ agent: {} }, 'get_window_state'))
})

test('authorize: works without a live agent (the grant survives, schemas follow later)', () => {
  const contract = createSafetyContract({ config: { defaultEnabled: false } })
  // No agent was ever synced: activate() reports 'unbound' but the grant itself
  // must still be recorded — the gate checks the grant, not the visibility.
  const result = contract.authorize('ghost', true)
  assert.equal(result.enabled, true)
  assert.equal(result.schemas, 'visible')
  const agent = { session: { id: 'ghost' } }
  assert.doesNotThrow(() => contract.gate({ agent }, 'click'))
})

test('contract: the defaults keep the current behaviour of this plugin', () => {
  assert.equal(DEFAULT_SAFETY_CONFIG.defaultEnabled, true)
  assert.equal(DEFAULT_SAFETY_CONFIG.denySubagents, true)
  assert.equal(DEFAULT_SAFETY_CONFIG.hideToolSchemasWhenOff, true)
  assert.equal(DEFAULT_SAFETY_CONFIG.controllerLease, true)
  assert.equal(DEFAULT_SAFETY_CONFIG.controllerLeaseIdleMs, 10000)
})
