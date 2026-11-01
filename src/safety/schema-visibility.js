/**
 * 会话的 schema 可见性（SEC-02）：能力关着时，让这个会话的工具**不占它的 schema 预算**。
 *
 * 移植自源项目 `git-repo/dsh-computer-use/internal/schema-visibility.mjs`
 * （见 `docs/design-essence-dsh-computer-use.md` §2 SEC-02）。
 *
 * 为什么值得做：已注册工具的 schema 随该会话的**每个请求**发出去。
 * 一个「这个会话根本不能调」的工具是纯开销——源项目实测那一族占了每请求
 * 20,990 个 schema token 中的 15,054。做法是 `tools.restrict` 只把工具从
 * **这一个 agent 的视野**里摘掉：定义仍然注册着、调用照样被拒，开关一打开立刻恢复。
 *
 * 两个真实的坑（源项目踩过，这里照抄解法）：
 *
 * 1. **释放姿势**：宿主 `tools.restrict()` 返回的是**函数**（cordis 的 `ctx.effect(fn)`
 *    返回 `Disposable<T> = () => T`），身上既没有 `.dispose` 也没有 `[Symbol.dispose]`。
 *    所以 `restricted.dispose?.()` 是**静默空操作**——限制挂上去就再也摘不下来，
 *    而账本已经记成「恢复可见」：**芯片是亮的、工具永远不出现**。
 *    这里把释放单独做成 {@link liftRestriction}，两种形态都认。
 *
 * 2. **不认识的名字会让整条限制失败**：`tools.restrict({ deny })` 对任何一个
 *    在这个作用域里不可限制的名字都会**整条**抛错。所以这里先整批试，
 *    失败后退化成**逐个名字**试——能限制几个算几个，而不是全盘放弃。
 *
 * 另有一条纪律：**任何一次 `sync` 都是一次真实的接触，所以接触即登记**。
 * 只靠一个事件面（`agent/created`）去登记活 agent 是不够的：会话恢复、事件面缺失、
 * 插件晚于 agent 挂载这三种时序都会让那次登记错过，于是限制挂上却再没人摘得下来。
 */

/**
 * 摘掉一条限制。**这是本模块唯一正确的释放姿势**，所以它单独成函数、两个调用点共用。
 *
 * 两种形态都认（函数优先，顺带兼容 `.dispose()` 对象形态的旧宿主）。
 *
 * @param {*} disposable - `tools.restrict()` 的返回值。
 */
export function liftRestriction(disposable) {
  if (typeof disposable === 'function') {
    disposable()
    return
  }
  if (typeof disposable?.dispose === 'function') {
    disposable.dispose()
  }
}

/**
 * @typedef {object} SchemaVisibility
 * @property {(agent: object) => string} sync - 让一个 agent 的工具可见性对齐它会话当前的授权（幂等）。
 * @property {(sessionId: unknown) => string} activate - 用户把能力点开：让这个会话的工具立刻回来。
 * @property {(sessionId: unknown) => string} deactivate - 用户把能力关掉：让这个会话重新为它的 schema 付费。
 * @property {(key: string) => void} release - 撤掉一个会话持有的限制。
 * @property {(agent: object) => void} remember - agent 创建：记下它并同步一次。
 * @property {(agent: object) => void} forget - agent 销毁：清掉它的限制与登记。
 * @property {() => void} releaseAll - 插件卸载：把还留着的限制全部撤掉。
 * @property {(key: string) => string} state - 控制块要的那一格。
 * @property {() => number} liveAgentCount - 现在有多少个活 agent 登记在案。
 * @property {() => number} restrictedCount - 现在有多少个会话的工具 schema 正被摘掉。
 */

/**
 * @param {object} options
 * @param {(sessionId: unknown) => boolean} options.isEnabled - 问这个会话的能力开着没有。
 * @param {() => string[]} [options.hideableNames] - 能力关着时可以摘掉的工具名。
 *   做成取值函数是因为浏览器工具可能在**之后**才注册进来。
 * @param {boolean} [options.enabled] - 配置项 `hideToolSchemasWhenOff !== false`；
 *   false 表示保持「一直可见」。
 * @param {object} [options.logger] - 可选的 ctx.logger。
 * @returns {SchemaVisibility}
 */
export function createSchemaVisibility(options = {}) {
  const enabled = options.enabled !== false
  const isEnabled = typeof options.isEnabled === 'function' ? options.isEnabled : () => true
  const hideableNames = typeof options.hideableNames === 'function'
    ? options.hideableNames
    : () => (Array.isArray(options.hideableNames) ? options.hideableNames : [])
  const logger = options.logger

  /** sessionKey -> `agent.ctx.tools.restrict()` 返回的 disposer（**函数**，见 {@link liftRestriction}）。 */
  const hidden = new Map()
  /** sessionKey -> 活着的 agent，好让「点亮能力」唯一能找到 agent 的路径。 */
  const liveAgents = new Map()
  /** sessionKey -> 最近一次应用的可见性状态。 */
  const states = new Map()

  /**
   * 把工具从这一个 agent 的视野里摘掉。
   *
   * @param {object} agent
   * @param {string[]} names
   * @returns {(() => void) | null} 合并后的 disposer；null 表示摘不掉（保持「可见但被拒」）。
   */
  function applyRestriction(agent, names) {
    const tools = agent?.ctx?.tools
    if (tools === undefined || typeof tools.restrict !== 'function') return null
    if (names.length === 0) return null
    try {
      return tools.restrict({ deny: names })
    } catch (error) {
      // 整批失败：很可能只是因为其中一个名字在这个作用域里不可限制。
      // 逐个再试一次——能限制几个算几个，别因为一个名字全盘放弃。
      const disposers = []
      for (const name of names) {
        try {
          disposers.push(tools.restrict({ deny: [name] }))
        } catch {
          /* 这个名字在这个作用域里不可限制，跳过它 */
        }
      }
      if (disposers.length === 0) {
        logger?.warn?.(
          'dsh-computer-use: could not hide tool schemas: %s',
          String(error),
        )
        return null
      }
      return () => {
        for (const disposer of disposers) liftRestriction(disposer)
      }
    }
  }

  /**
   * 让一个 agent 的工具可见性对齐它会话当前的授权。
   *
   * 幂等：agent 创建时、每次能力开关、以及每个工具调用里的授权检查都会调它，
   * 所以一个早于本插件监听器存在的会话，也会在它下一次调用时收敛。
   *
   * @param {object} agent - 要同步的活 agent。
   * @returns {'advertised' | 'visible' | 'hidden'}
   */
  function sync(agent) {
    // 同步一个 agent 的同时把它登记下来：任何一次 sync 都是一次真实的接触，
    // 所以接触即登记，比只靠一个事件面结实。
    const key = sessionKeyOf(agent)
    if (key !== null) liveAgents.set(key, agent)
    if (!enabled) return 'advertised'
    if (isEnabled(agent?.session?.id ?? agent?.id)) {
      const restricted = hidden.get(key)
      if (restricted !== undefined) {
        try {
          liftRestriction(restricted)
        } catch (error) {
          logger?.warn?.('dsh-computer-use: releasing the tool restriction failed: %s', String(error))
        }
        hidden.delete(key)
        states.set(key, 'visible')
      }
      return 'visible'
    }
    if (hidden.has(key)) return 'hidden'
    const disposer = applyRestriction(agent, hideableNames())
    if (disposer === null) {
      // 没有带作用域的 tools 服务、或每个名字都被拒绝，都不该把能力本身弄坏：
      // 工具保持「可见但被拒」。
      states.set(key, 'visible')
      return 'visible'
    }
    hidden.set(key, disposer)
    states.set(key, 'hidden')
    logger?.info?.(
      'dsh-computer-use: session %s hides %d tool schema(s) while 电脑操作 is off',
      key,
      hideableNames().length,
    )
    return 'hidden'
  }

  /**
   * 用户把能力点开：让这个会话的工具立刻回来。
   *
   * 与直接 `sync(agentOf(key))` 的差别只有一处，但正是出事的那一处：
   * **找不到 agent 时不再什么都不做**。限制的 disposer 不依赖 agent，
   * 留着它只会让「用户已经说了可以」这一个事实被本地记账吃掉，
   * 于是能力是开的、工具永远不出现。找不到 agent 就地释放，
   * agent 之后出现时 `sync` 会按同一个授权值重新算——开着就不会再挂回去。
   *
   * @param {unknown} sessionId
   * @returns {'visible' | 'advertised' | 'unbound'}
   */
  function activate(sessionId) {
    const key = String(sessionId ?? '')
    const agent = liveAgents.get(key)
    if (agent !== undefined && enabled) {
      const next = sync(agent)
      if (next !== 'hidden') return next
    }
    release(key)
    return agent === undefined ? 'unbound' : enabled ? 'visible' : 'advertised'
  }

  /**
   * 用户把能力关掉：让这个会话重新为它的 schema 付费。
   *
   * @param {unknown} sessionId
   * @returns {'visible' | 'hidden' | 'unbound'}
   */
  function deactivate(sessionId) {
    const key = String(sessionId ?? '')
    const agent = liveAgents.get(key)
    if (agent !== undefined) return sync(agent)
    release(key)
    return 'unbound'
  }

  /**
   * 撤掉一个会话持有的限制（会话销毁、能力关闭、插件卸载时）。
   * @param {string} key
   */
  function release(key) {
    const restricted = hidden.get(key)
    if (restricted === undefined) return
    try {
      liftRestriction(restricted)
    } catch (error) {
      logger?.warn?.('dsh-computer-use: releasing the tool restriction failed: %s', String(error))
    }
    hidden.delete(key)
    states.set(key, 'visible')
  }

  /**
   * agent 创建：记下它，并同步一次。
   * @param {object} agent
   */
  function remember(agent) {
    if (agent?.session?.id === undefined && agent?.id === undefined) return
    sync(agent)
  }

  /**
   * agent 销毁：清掉它的限制与登记。
   * @param {object} agent
   */
  function forget(agent) {
    const key = sessionKeyOf(agent)
    if (key === null) return
    liveAgents.delete(key)
    release(key)
    states.delete(key)
  }

  /** 插件卸载：把还留着的限制全部撤掉。 */
  function releaseAll() {
    for (const key of [...hidden.keys()]) release(key)
    liveAgents.clear()
  }

  /**
   * 控制块要的那一格：`'hidden'` / `'visible'`（配置关掉时为 `'advertised-by-config'`）。
   * @param {string} key
   * @returns {string}
   */
  function state(key) {
    if (!enabled) return 'advertised-by-config'
    return hidden.has(String(key ?? '')) ? 'hidden' : 'visible'
  }

  return {
    enabled,
    sync,
    activate,
    deactivate,
    release,
    remember,
    forget,
    releaseAll,
    state,
    agentOf: key => liveAgents.get(String(key ?? '')),
    liveAgentCount: () => liveAgents.size,
    restrictedCount: () => hidden.size,
  }
}

/**
 * 一个 agent 的会话键；拿不到就返回 null（不登记，也不限制）。
 *
 * @param {object} agent
 * @returns {string | null}
 */
function sessionKeyOf(agent) {
  const id = agent?.session?.id ?? agent?.id
  if (id === undefined || id === null) return null
  return String(id)
}
