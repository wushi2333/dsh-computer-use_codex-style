/**
 * 失败的结构化分类学：`code` + `retry` + 「下一步做什么」。
 *
 * 移植自源项目 `git-repo/dsh-computer-use/internal/failures.mjs` 的设计精华（见
 * `docs/design-essence-dsh-computer-use.md` §3 E2）。核心判断：
 *
 * > 「它失败了」和「它失败了，先重新观察一下」需要**相反**的反应。
 *
 * 所以每条失败规则除了一个稳定的 `code`，还给出一个 `retry` 判据：
 *
 * | retry      | 含义                                                              |
 * | ---------- | ----------------------------------------------------------------- |
 * | `never`    | 重发没有意义（目标根本不支持这个动作 / 状态不对），只烧 turn      |
 * | `retry`    | 可以直接重发                                                       |
 * | `reobserve`| 先重新观察目标，再决定要不要重发                                   |
 *
 * 没列进表里的失败一律按 `reobserve` 处理——**宁可多看一眼，也不要重复一次副作用**。
 *
 * 这张表是**纯数据 + 纯函数**，没有任何宿主依赖，因此可以在 `node --test` 下直接单测。
 */

/** `retry` 判据的三个取值。 */
export const RETRY = Object.freeze({
  /** 重发没有意义。 */
  NEVER: 'never',
  /** 可以直接重发。 */
  RETRY: 'retry',
  /** 先重新观察目标，再决定要不要重发。 */
  REOBSERVE: 'reobserve',
})

/**
 * 有序的失败判据表。**顺序即优先级，第一条命中即用。**
 *
 * 每条规则给出三个字段：
 * - `code`   稳定错误码（模型、doctor、日志都按它归类）；
 * - `retry`  见 {@link RETRY}；
 * - `hint`   下一步做什么。`retry: 'never'` 的 `hint` 尤其重要：它告诉模型
 *            「不要重发，改走哪条路」，而不是让模型自己猜。
 *
 * @type {ReadonlyArray<{ code: string, retry: string, test: RegExp, hint: string }>}
 */
export const FAILURE_RULES = Object.freeze([
  {
    code: 'UNSUPPORTED_PLATFORM',
    retry: RETRY.NEVER,
    test: /\[UNSUPPORTED_PLATFORM\]/,
    hint: '电脑操作只在 Windows 上实现（SendInput + GDI + UI Automation）。当前平台没有这套后端，'
      + '这不是一次可以重试的失败；换用平台自带的工具，或者在 Windows 上运行。',
  },
  {
    code: 'SUBAGENT_COMPUTER_USE_UNAVAILABLE',
    retry: RETRY.NEVER,
    test: /\[SUBAGENT_COMPUTER_USE_UNAVAILABLE\]/,
    hint: '子代理不能驱动桌面：子代理的权限范围在启动时就固定了，不能从会话内部放宽。'
      + '请把需要操作电脑的这一步交回主代理执行。',
  },
  {
    code: 'NOT_AUTHORIZED',
    retry: RETRY.NEVER,
    test: /\[NOT_AUTHORIZED\]/,
    hint: '电脑操作尚未在本会话开启。请让用户在会话里开启「电脑操作」能力后重试；'
      + '在此之前读操作同样不可用，所以先不要再重试这一族工具。',
  },
  {
    code: 'CONTROLLER_BUSY',
    retry: RETRY.RETRY,
    test: /\[CONTROLLER_BUSY\]/,
    hint: '另一个会话正在驱动同一只鼠标和键盘——一只鼠标没法同时服务两个代理。'
      + '按错误里给出的等待时间重试；读操作（截图、读窗口）在等待期间仍然可用。',
  },
  {
    code: 'SCREEN_LOCKED',
    retry: RETRY.NEVER,
    test: /\[SCREEN_LOCKED\]|desktop is locked|input desktop is |workstation is locked|OpenInputDesktop failed|secure desktop/i,
    hint: '当前不可交互（锁屏或安全桌面）。请先让用户解锁，再重试。',
  },
  {
    code: 'ELEVATION_BLOCKED',
    retry: RETRY.NEVER,
    test: /\[ELEVATION_BLOCKED\]|higher Windows integrity|integrity than the Computer Use/i,
    hint: '目标窗口以更高的完整性级别运行（提权/UIPI），合成输入到不了它。'
      + '请改用 UIA 通道（get_window_state + set_value / perform_secondary_action），那条通道不受 UIPI 限制。',
  },
  {
    code: 'OBSERVATION_STALE',
    retry: RETRY.REOBSERVE,
    test: /no fresh get_window_state|observation expired|is not the latest observation/i,
    hint: '这次输入缺少一次新鲜的观察。先 get_window_state 重新观察目标窗口，再动作。',
  },
  {
    code: 'TURN_INTERRUPTED',
    retry: RETRY.NEVER,
    test: /\[INTERRUPTED\]|stopped by the user|physical Escape/i,
    hint: '这一轮被用户中断了（物理 Escape / 点停止）。先观察屏幕确认当前状态，'
      + '再决定要不要继续——中断发生在哪一步并不确定。',
  },
  {
    code: 'APPROVAL_DENIED',
    retry: RETRY.NEVER,
    test: /ApprovalDenied|was not approved/i,
    hint: '这次操作没有被批准，而且批准不会自动到来。请改用别的做法，或者请用户先批准。',
  },
])

/** 未命中任何规则时的兜底判据。 */
export const DEFAULT_FAILURE = Object.freeze({
  code: 'UNKNOWN',
  retry: RETRY.REOBSERVE,
  hint: '先观察目标当前的状态（截图 / 读窗口），确认上一次调用到底有没有生效，再决定要不要重发。',
})

/**
 * 给一个失败贴上稳定的 `code` 与 `retry` 判据。
 *
 * @param {unknown} error - 抛出的值（Error、字符串、任意对象）。
 * @returns {{ code: string, retry: string, hint: string, message: string }}
 */
export function decorateFailure(error) {
  const message = error instanceof Error ? error.message : String(error ?? '')
  for (const rule of FAILURE_RULES) {
    if (rule.test.test(message)) {
      return { code: rule.code, retry: rule.retry, hint: rule.hint, message }
    }
  }
  return { code: DEFAULT_FAILURE.code, retry: DEFAULT_FAILURE.retry, hint: DEFAULT_FAILURE.hint, message }
}

/**
 * 安全契约抛出的错误。
 *
 * 带上 `code` / `retry` 两个结构化字段，所以调用方不必再去猜一条消息的含义；
 * 同时把 `code` 写进消息文本（`[CODE] …`），于是**日志、模型看到的文本、
 * 以及结构化字段**说的是同一件事——这是源项目 `requireGrant` 的做法。
 */
export class SafetyError extends Error {
  /**
   * @param {string} code - 稳定错误码。
   * @param {string} text - 面向模型的一句话说明。
   * @param {{ retry?: string, hint?: string, cause?: unknown }} [options]
   */
  constructor(code, text, options = {}) {
    super(`[${code}] ${text}`)
    this.name = 'SafetyError'
    this.code = code
    this.retry = options.retry ?? RETRY.REOBSERVE
    if (options.hint !== undefined) this.hint = options.hint
    if (options.cause !== undefined) this.cause = options.cause
  }
}

/**
 * 抛出一条带 `code` 的安全拒绝，并附上「下一步做什么」。
 *
 * @param {string} code - {@link FAILURE_RULES} 里的稳定错误码。
 * @param {string} text - 面向模型的一句话说明。
 * @param {{ cause?: unknown }} [options]
 * @returns {never} 总是抛出。
 */
export function refuse(code, text, options = {}) {
  const rule = FAILURE_RULES.find(entry => entry.code === code)
  const retry = rule?.retry ?? DEFAULT_FAILURE.retry
  const hint = rule?.hint ?? DEFAULT_FAILURE.hint
  throw new SafetyError(code, `${text}\n${hint}`, { retry, hint, ...options })
}
