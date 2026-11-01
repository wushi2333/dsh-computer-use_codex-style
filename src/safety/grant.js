/**
 * 会话授权表（SEC-01）：能力默认关闭，按会话授权。
 *
 * 移植自源项目 `git-repo/dsh-computer-use/internal/sessions.mjs` + `index.js:641-669`
 * 的设计精华（见 `docs/design-essence-dsh-computer-use.md` §2 SEC-01）。
 *
 * 三条不变量：
 *
 * 1. **授权的粒度是会话，不是部署。** `defaultEnabled` 只是「从未被显式授权过的会话」的兜底值，
 *    不是全局开关。显式 `true` > 显式 `false` > `defaultEnabled`。
 * 2. **显式「关」必须能压住默认值。** 所以查表用 `??` 而不是 `||`：
 *    `false || true` 会把「显式关闭」错当成「没有记录」。
 * 3. **匿名会话也要走同一套门禁。** 拿不到 session id 时归一化成 `''`，
 *    它仍然被同一张表管着——不是「没有 id 就绕过门禁」。
 *
 * 另有一条**不可从会话内部放宽**的规则：**子代理一律拒绝**。子代理的权限范围在启动时就固定，
 * 从会话内部放宽它等于让「默认开启」这一条部署设置把所有子代理一起放进来。
 */
import { refuse } from './failures.js'

/** 匿名会话的归一化键。 */
export const ANONYMOUS_SESSION = ''

/**
 * 归一化会话标识。
 *
 * `undefined` / `null` → `''`（匿名会话）；其余走 `String(...)`。
 * 归一化不属于某个实例，所以它是模块级导出——授权查询才在实例上。
 *
 * @param {unknown} sessionId
 * @returns {string}
 */
export function sessionKey(sessionId) {
  return sessionId === undefined || sessionId === null ? ANONYMOUS_SESSION : String(sessionId)
}

/**
 * 这个 agent 是不是一个被委派的子代理。
 *
 * 判据是 `session.header.origin === 'subagent'`。DSH 的子代理会话带着这个 header，
 * 它的权限范围在**启动时**就固定了，不能在会话内部放宽。
 *
 * @param {object} [agent] - 工具执行上下文里的 agent。
 * @returns {boolean}
 */
export function isSubagentSession(agent) {
  const header = agent?.session?.header
  return header?.origin === 'subagent'
}

/**
 * 一张按会话的授权表。
 *
 * @param {{ defaultEnabled?: boolean }} [options]
 * @returns {{
 *   isEnabled: (sessionId: unknown) => boolean,
 *   grant: (sessionId: unknown) => void,
 *   revoke: (sessionId: unknown) => void,
 *   hasExplicit: (sessionId: unknown) => boolean,
 *   forget: (sessionId: unknown) => void,
 *   defaultEnabled: boolean,
 *   sessions: () => Array<{ sessionId: string, enabled: boolean }>,
 *   report: (sessionId: unknown) => { sessionId: string, enabled: boolean, explicit: boolean },
 * }}
 */
export function createGrantStore(options = {}) {
  const defaultEnabled = options.defaultEnabled === true
  /** sessionKey -> 显式授权（true = 授权，false = 收回）。没有记录 = 用 defaultEnabled。 */
  const grants = new Map()

  /**
   * 这个会话现在能不能用电脑操作。
   * @param {unknown} sessionId
   * @returns {boolean}
   */
  function isEnabled(sessionId) {
    const recorded = grants.get(sessionKey(sessionId))
    // `??` 而不是 `||`：显式的 false 必须压住 defaultEnabled。
    return recorded ?? defaultEnabled
  }

  /**
   * 授权一个会话。
   * @param {unknown} sessionId
   */
  function grant(sessionId) {
    grants.set(sessionKey(sessionId), true)
  }

  /**
   * 收回一个会话的授权。
   * @param {unknown} sessionId
   */
  function revoke(sessionId) {
    grants.set(sessionKey(sessionId), false)
  }

  /**
   * 这个会话有没有被显式授权或收回过。
   * @param {unknown} sessionId
   * @returns {boolean}
   */
  function hasExplicit(sessionId) {
    return grants.has(sessionKey(sessionId))
  }

  /**
   * 忘掉一个会话的记录（会话销毁时调用）。之后它回到部署默认值。
   * @param {unknown} sessionId
   */
  function forget(sessionId) {
    grants.delete(sessionKey(sessionId))
  }

  /** 列出所有显式记录过的会话（给 health 报告用）。 */
  function sessions() {
    return [...grants.entries()].map(([key, enabled]) => ({ sessionId: key, enabled }))
  }

  /**
   * 某个会话的授权状态快照。
   * @param {unknown} sessionId
   * @returns {{ sessionId: string, enabled: boolean, explicit: boolean }}
   */
  function report(sessionId) {
    const key = sessionKey(sessionId)
    return { sessionId: key, enabled: isEnabled(sessionId), explicit: grants.has(key) }
  }

  return { isEnabled, grant, revoke, hasExplicit, forget, sessions, report, defaultEnabled }
}

/**
 * 每个工具入口的第一道门禁。
 *
 * 顺序是刻意的：
 * 1. 平台不支持 —— 先说清楚这不是一次可以重试的失败；
 * 2. 子代理 —— **在授权检查之前**，所以一个被授权的子代理拿到的仍然是
 *    `SUBAGENT_COMPUTER_USE_UNAVAILABLE`（"你不该驱动桌面"）而不是
 *    `NOT_AUTHORIZED`（"你还没被授权"）：前者告诉他该做什么，后者只会让他去点开关。
 * 3. 会话授权 —— 关着的话，收敛一次 schema 可见性（让这个会话不再为用不了的工具付费），
 *    然后拒绝。
 *
 * @param {object} params
 * @param {{ isEnabled: (sessionId: unknown) => boolean }} params.grant - 授权表。
 * @param {object} [params.agent] - 工具执行上下文里的 agent。
 * @param {unknown} [params.sessionId] - 会话 id（缺省从 agent 上取）。
 * @param {boolean} [params.denySubagents] - 是否拒绝子代理（默认 true）。
 * @param {boolean} [params.supported] - 当前平台是否支持（默认 true）。
 * @param {string} [params.unsupportedReason] - 不支持时的说明。
 * @param {(agent: object) => void} [params.onNotAuthorized] - 授权被拒时的回调（用于收敛 schema 可见性）。
 * @returns {void}
 */
export function requireGrant(params) {
  const {
    grant,
    agent,
    denySubagents = true,
    supported = true,
    unsupportedReason = '电脑操作目前只实现了 Windows（SendInput + GDI + UI Automation）。',
    onNotAuthorized,
  } = params
  if (!supported) {
    refuse('UNSUPPORTED_PLATFORM', unsupportedReason)
  }
  if (denySubagents && isSubagentSession(agent)) {
    refuse(
      'SUBAGENT_COMPUTER_USE_UNAVAILABLE',
      '电脑操作不对子代理开放：子代理的权限范围在启动时就固定了，不能从会话内部放宽。',
    )
  }
  const sessionId = params.sessionId !== undefined
    ? params.sessionId
    : agent?.session?.id !== undefined
      ? agent.session.id
      : agent?.id
  if (!grant.isEnabled(sessionId)) {
    // 收敛一次：让这个会话不再为它用不了的工具 schema 付费。
    if (typeof onNotAuthorized === 'function') {
      try {
        onNotAuthorized(agent)
      } catch {
        // 收敛失败不该把能力本身弄坏：工具保持「可见但被拒」。
      }
    }
    refuse(
      'NOT_AUTHORIZED',
      '电脑操作尚未在本会话开启。请让用户在本会话开启「电脑操作」能力，然后重试。',
    )
  }
}
