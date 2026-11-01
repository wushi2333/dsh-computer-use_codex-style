/**
 * 安全契约（Safety Contract）：把五条安全条款装成一件可审计的产品。
 *
 * 移植自源项目 `git-repo/dsh-computer-use` 的设计精华（见
 * `docs/design-essence-dsh-computer-use.md`）。这个文件是**装配层**：
 * 它把四个互不依赖的模块（授权 / schema 可见性 / 写租约 / 失败分类学）
 * 接到一起，并给出唯一一个对外的入口 `gate()`。
 *
 * 装配层的存在意义只有一个：**让「能力能不能用」「用了付出什么代价」
 * 「失败了怎么办」这三件事有唯一的答案**。源项目的教训是
 * 「曾经有三份并且互相矛盾」——所以这里只有一份。
 */
import { createGrantStore, requireGrant, sessionKey, isSubagentSession } from './grant.js'
import { createSchemaVisibility } from './schema-visibility.js'
import { createControllerLease, movesDesktop } from './controller-lease.js'
import { decorateFailure, FAILURE_RULES, RETRY, SafetyError, refuse } from './failures.js'

export { createGrantStore, requireGrant, sessionKey, isSubagentSession } from './grant.js'
export { createSchemaVisibility, liftRestriction } from './schema-visibility.js'
export { createControllerLease, movesDesktop, WRITE_TOOLS, CONTROLLER_IDLE_MS } from './controller-lease.js'
export { decorateFailure, FAILURE_RULES, RETRY, SafetyError, refuse } from './failures.js'

/**
 * 安全契约的默认配置。
 *
 * 全部保持**现状**：本项目现有行为不变。
 *
 * `defaultEnabled: true` 是与源项目的唯一分歧——源项目是「默认关闭」。
 * 差别的原因很实际：本项目没有输入框里的「电脑操作」芯片（没有前端行），
 * 所以「默认关闭」在这里等于「默认没有功能」。
 * 真正的源项目语义（每会话一个开关、关着时摘 schema）由
 * `grant` / `revoke` 这两个 API 提供，宿主随时可以对某个会话调用。
 *
 * @type {{
 *   defaultEnabled: boolean,
 *   denySubagents: boolean,
 *   hideToolSchemasWhenOff: boolean,
 *   controllerLease: boolean,
 *   controllerLeaseIdleMs: number,
 * }}
 */
export const DEFAULT_SAFETY_CONFIG = Object.freeze({
  defaultEnabled: true,
  denySubagents: true,
  hideToolSchemasWhenOff: true,
  controllerLease: true,
  controllerLeaseIdleMs: 10_000,
})

/**
 * 装一份安全契约。
 *
 * @param {object} [options]
 * @param {Partial<typeof DEFAULT_SAFETY_CONFIG>} [options.config] - 配置覆盖。
 * @param {object} [options.logger] - 可选的 ctx.logger。
 * @param {() => string[]} [options.hideableNames] - 能力关着时可以摘掉的工具名（取值函数）。
 * @param {boolean} [options.supported] - 当前平台是否支持电脑操作。
 * @param {string} [options.unsupportedReason] - 不支持时的说明。
 * @returns {{
 *   grant: object,
 *   lease: object,
 *   visibility: object,
 *   gate: (exec: object, toolName: string) => { sessionId: string, write: boolean },
 *   authorize: (sessionId: unknown, enabled: boolean) => object,
 *   beginWrite: () => void,
 *   finishWrite: () => void,
 *   controlState: (sessionId: unknown) => object,
 *   report: (sessionId: unknown) => object,
 *   setToolNames: (names: string[]) => void,
 *   forgetSession: (sessionId: unknown) => void,
 *   releaseAll: () => void,
 * }}
 */
export function createSafetyContract(options = {}) {
  const config = { ...DEFAULT_SAFETY_CONFIG, ...(options.config ?? {}) }
  const logger = options.logger
  const supported = options.supported !== false
  const unsupportedReason = options.unsupportedReason
    ?? '电脑操作目前只实现了 Windows（SendInput + GDI + UI Automation）。'

  const grant = createGrantStore({ defaultEnabled: config.defaultEnabled })
  const lease = createControllerLease({ idleMs: config.controllerLeaseIdleMs })

  /** 工具行注册完之后把名字报过来（浏览器工具会在之后才注册）。 */
  let toolNames = []
  const hideableNames = () => {
    const configured = typeof options.hideableNames === 'function' ? options.hideableNames() : toolNames
    return Array.isArray(configured) ? configured.filter(name => typeof name === 'string' && name) : []
  }

  const visibility = createSchemaVisibility({
    enabled: config.hideToolSchemasWhenOff,
    isEnabled: sessionId => grant.isEnabled(sessionId),
    hideableNames,
    logger,
  })

  /**
   * 每个工具调用的第一道门禁。
   *
   * 顺序：平台 → 子代理 → 会话授权 → 跨会话写租约。
   *
   * @param {object} exec - 工具执行上下文（`agent` / `callId` / `signal`）。
   * @param {string} toolName - 即将调用的工具名。
   * @returns {{ sessionId: string, write: boolean }}
   */
  function gate(exec, toolName) {
    const agent = exec?.agent
    const sessionId = agent?.session?.id ?? agent?.id
    requireGrant({
      grant,
      agent,
      sessionId,
      denySubagents: config.denySubagents,
      supported,
      unsupportedReason,
      // 授权被拒时顺手收敛一次 schema 可见性：让这个会话不再为
      // 它用不了的工具付费。收敛失败不该把能力本身弄坏。
      onNotAuthorized: target => visibility.sync(target),
    })
    const write = movesDesktop(toolName)
    if (config.controllerLease && write) {
      lease.claim(sessionId, toolName)
    }
    return { sessionId: sessionId === undefined || sessionId === null ? '' : String(sessionId), write }
  }

  /** 一个写操作开始执行：桌面此刻正在被驱动。 */
  function beginWrite() {
    if (config.controllerLease) lease.beginWrite()
  }

  /** 一个写操作结束：空闲时钟从这里重新开始。 */
  function finishWrite() {
    if (config.controllerLease) lease.finishWrite()
  }

  /**
   * 控制块的状态（对齐源项目 `controlState` 的六个状态）。
   *
   * @param {unknown} sessionId
   * @returns {{ state: string, enabled: boolean, explicit: boolean, toolSchemas: string, lease: object }}
   */
  function controlState(sessionId) {
    const enabled = grant.isEnabled(sessionId)
    const explicit = grant.hasExplicit(sessionId)
    const leaseReport = lease.report(sessionId)
    let state = 'off'
    if (!enabled) state = explicit ? 'stopped' : 'off'
    else if (leaseReport.owns && leaseReport.idleMs !== null && leaseReport.idleMs < lease.idleMs) state = 'ready'
    else state = 'observing'
    return {
      state,
      enabled,
      explicit,
      toolSchemas: visibility.state(sessionKey(sessionId)),
      lease: leaseReport.lease,
    }
  }

  /**
   * 一份可审计的安全状态快照。
   * @param {unknown} sessionId
   * @returns {object}
   */
  function report(sessionId) {
    return {
      config: { ...config },
      control: controlState(sessionId),
      schemaVisibility: {
        enabled: visibility.enabled,
        liveAgents: visibility.liveAgentCount(),
        restrictedSessions: visibility.restrictedCount(),
      },
      sessions: grant.sessions(),
    }
  }

  /**
   * 工具行把注册好的工具名报过来。
   * @param {string[]} names
   */
  function setToolNames(names) {
    toolNames = Array.isArray(names) ? names.filter(name => typeof name === 'string' && name) : []
  }

  /**
   * 会话结束：撤掉它的授权、让出租约、摘掉它的 schema 限制。
   * @param {unknown} sessionId
   */
  function forgetSession(sessionId) {
    grant.forget(sessionId)
    lease.release(sessionId)
    visibility.release(sessionKey(sessionId))
  }

  /**
   * 一把会话内不可自取的钥匙：从**会话之外**把能力开给一个会话（或关掉）。
   *
   * 这是 `grant.grant()` / `grant.revoke()` 的可调用形态——没有它，
   * `safetyDefaultEnabled: false` 的部署没有任何路径能把会话重新打开。
   * 「会话内不可自取」由调用方保证：这个方法只该被宿主（用户操作、服务 API）
   * 调用，不该从工具执行路径里够到。
   *
   * @param {unknown} sessionId
   * @param {boolean} enabled - true = 授权，false = 收回。
   * @returns {{ sessionId: string, enabled: boolean, explicit: boolean, schemas: string }}
   */
  function authorize(sessionId, enabled) {
    const next = enabled === true
    if (next) grant.grant(sessionId)
    else grant.revoke(sessionId)
    // 授权状态变了，schema 可见性必须立刻跟上：开着就把工具放回来，
    // 关掉就让这个会话重新为它付 schema 费。找不到活 agent 时就地释放
    // （`activate`/`deactivate` 的语义），agent 之后出现时 `sync` 会按
    // 同一个授权值重新算。
    if (next) visibility.activate(sessionId)
    else visibility.deactivate(sessionId)
    return {
      sessionId: sessionKey(sessionId),
      enabled: next,
      explicit: grant.hasExplicit(sessionId),
      schemas: visibility.state(sessionKey(sessionId)),
    }
  }

  /** 插件卸载：把还留着的限制全部撤掉。 */
  function releaseAll() {
    visibility.releaseAll()
  }

  return {
    config,
    grant,
    lease,
    visibility,
    gate,
    authorize,
    beginWrite,
    finishWrite,
    controlState,
    report,
    setToolNames,
    forgetSession,
    releaseAll,
  }
}
