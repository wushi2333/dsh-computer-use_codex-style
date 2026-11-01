/**
 * 跨会话的控制权租约（SEC-03）：同一时刻只让一个会话驱动桌面。
 *
 * 移植自源项目 `git-repo/dsh-computer-use/internal/lease.mjs`（见
 * `docs/design-essence-dsh-computer-use.md` §2 SEC-03）。
 *
 * 为什么需要它：本项目的 helper 是**进程级**的，所有会话共用同一只鼠标和键盘。
 * 两个会话同时动手，会交织成双方都无法解释的状态——截图对不上、点击落在别人的窗口上，
 * 而且**事后无法归因**（谁点的？）。这跟「模型不够聪明」无关，是物理事实。
 *
 * 三条规则：
 *
 * 1. **只有会移动桌面的调用才需要租约。** 读操作永远不需要——
 *    读和看可以并行，移动不行。
 * 2. **租约在写操作真正执行期间保持。** `beginWrite()` / `finishWrite()` 计数，
 *    **空闲时钟在写操作结束那一刻重新开始**。早期版本在「调用开始时」打戳、十秒后自行过期，
 *    于是一次长拖拽或含等待的批次还在跑，桌面就可能被交给另一个会话。
 * 3. **空闲即释放**，而不是要显式交还——会话之间交接就只是「停一下」，不需要任何仪式。
 *
 * 拒绝时要说清是「**正在执行**」还是「**安静了 N 秒**」：这两种情况的应对完全不同。
 */
import { refuse } from './failures.js'

/** 空闲多久之后租约自行释放（毫秒）。 */
export const CONTROLLER_IDLE_MS = 10_000

/**
 * 会移动桌面的工具。
 *
 * 读工具刻意不在其中：`get_window_state` / `list_windows` / `list_apps` / `get_window`
 * 以及诊断族（`session_note` / `session_state` / `diagnostic_state` / `end_turn`）
 * 与 `computer_use_health` 都不动桌面，永远不需要租约。
 *
 * `activate_window` 与 `launch_app` **在**其中：前者抢焦点，后者会改变桌面上的窗口集合——
 * 两者都会让另一个会话正在进行的坐标操作落到别的东西上。
 */
export const WRITE_TOOLS = Object.freeze(new Set([
  'click',
  'type_text',
  'press_key',
  'scroll',
  'scroll_element',
  'drag',
  'set_value',
  'perform_secondary_action',
  'activate_window',
  'launch_app',
  'fill_form',
  'batch_actions',
  'paste',
  'select_text',
]))

/**
 * 这个调用是「动桌面」还是「看桌面」。
 *
 * @param {string} toolName - 工具名。
 * @returns {boolean}
 */
export function movesDesktop(toolName) {
  return WRITE_TOOLS.has(String(toolName ?? ''))
}

/**
 * 一份按空闲计时的控制权租约。
 *
 * @param {{ idleMs?: number }} [options]
 * @returns {{
 *   claim: (sessionId: unknown, toolName: string) => void,
 *   beginWrite: () => void,
 *   finishWrite: () => void,
 *   note: (sessionId: unknown, what: string) => void,
 *   release: (sessionId: unknown) => void,
 *   report: (sessionId: unknown) => object,
 *   owner: () => string | null,
 *   idleMs: number,
 * }}
 */
export function createControllerLease(options = {}) {
  const idleMs = Number.isFinite(options.idleMs) && options.idleMs > 0 ? Number(options.idleMs) : CONTROLLER_IDLE_MS
  /** 当前持有租约的会话（`{ sessionId, at }`），没人持有时为 null。 */
  let controller = null
  /** 正在执行的写操作计数：>0 表示桌面**此刻**正在被驱动。 */
  let running = 0
  /** 这个会话最后一次动桌面的时间与内容，供状态报告展示。 */
  let lastAction = null

  /**
   * 取租约，或者因为别的会话正持有而拒绝。
   *
   * 同一个会话永远拿得到租约（重入）——一个会话内部的并发调用不该自己把自己挡住。
   *
   * @param {unknown} sessionId - 会话标识。
   * @param {string} toolName - 即将调用的工具名（读工具直接放行）。
   */
  function claim(sessionId, toolName) {
    if (!movesDesktop(toolName)) return
    const key = sessionId === undefined || sessionId === null ? '' : String(sessionId)
    const now = Date.now()
    if (controller !== null && controller.sessionId !== key) {
      const executing = running > 0
      const idleFor = Math.round((now - controller.at) / 1000)
      if (executing || now - controller.at < idleMs) {
        const owner = controller.sessionId === ''
          ? '(an unidentified session)'
          : `…${controller.sessionId.slice(-8)}`
        // 「还在执行」和「已经安静一会儿了」是两个不同的答复，应对方式也不同：
        // 前者必须等它跑完，后者基本就是马上。
        const why = executing
          ? 'is in the middle of a write right now (the desktop is being driven this instant)'
          : `drove the desktop ${idleFor}s ago`
        const wait = executing
          ? 'a few seconds'
          : `${Math.max(1, Math.ceil((idleMs - (now - controller.at)) / 1000))}s`
        refuse(
          'CONTROLLER_BUSY',
          `另一个会话（${owner}）${why}，而一只鼠标没法同时服务两个代理。`
          + `等待 ${wait} 让它跑完；若等不及，请让那个会话结束它的电脑操作轮次，`
          + '或由用户通过宿主服务收回那个会话的电脑操作授权（这是会话内部做不到的事）。'
          + '此期间的读操作（截图、读窗口）仍然可用。',
        )
      }
    }
    controller = { sessionId: key, at: now }
  }

  /**
   * 记下「这个会话刚动过桌面」（状态报告展示用）。
   * @param {unknown} sessionId
   * @param {string} what
   */
  function note(sessionId, what) {
    lastAction = {
      sessionId: sessionId === undefined || sessionId === null ? '' : String(sessionId),
      at: Date.now(),
      what,
    }
  }

  /** 一个写操作开始执行：桌面此刻正在被驱动。 */
  function beginWrite() {
    running += 1
  }

  /**
   * 一个写操作结束。
   *
   * 空闲时钟**从这里开始**，不是在调用被接受时开始：
   * 「安静了 N 秒」必须真的意味着桌面安静了 N 秒。
   */
  function finishWrite() {
    running = Math.max(0, running - 1)
    if (running === 0 && controller !== null) controller.at = Date.now()
  }

  /**
   * 主动让出租约（会话结束时调用）。
   * @param {unknown} sessionId
   */
  function release(sessionId) {
    const key = sessionId === undefined || sessionId === null ? '' : String(sessionId)
    if (controller !== null && controller.sessionId === key) controller = null
  }

  /** 当前持有租约的会话 id；没人持有时为 null。 */
  function owner() {
    return controller === null ? null : controller.sessionId
  }

  /**
   * 状态报告要的那一块。
   * @param {unknown} sessionId
   * @returns {{ owns: boolean, idleMs: number|null, held: boolean, lease: object|null, lastAction: object|null }}
   */
  function report(sessionId) {
    const key = sessionId === undefined || sessionId === null ? '' : String(sessionId)
    const owns = controller !== null && controller.sessionId === key
    const idleFor = controller === null ? null : Math.max(0, Date.now() - controller.at)
    return {
      owns,
      idleMs: idleFor,
      held: idleFor !== null && idleFor < idleMs,
      lease: controller === null
        ? null
        : {
          owner: controller.sessionId === '' ? null : `…${controller.sessionId.slice(-8)}`,
          idleMs: idleFor,
          held: idleFor !== null && idleFor < idleMs,
        },
      lastAction: lastAction === null || lastAction.sessionId !== key
        ? null
        : { what: lastAction.what, at: lastAction.at, ageMs: Math.max(0, Date.now() - lastAction.at) },
    }
  }

  return { claim, beginWrite, finishWrite, note, release, report, owner, idleMs }
}
