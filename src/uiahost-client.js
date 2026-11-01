// 常驻 UIA 服务的 Node 客户端：一次 spawn，之后每次请求 <100ms。
//
// 背景（实测 2026-11-01）：同一台机器、同一个 WMPF 窗口，
//   PowerShell 的 FindAll(Descendants)       42ms
//   net8.0-windows + UseWPF 的同一调用      106s   ← 托管包装在这个窗口上退化成逐节点跨进程查询
// 所以常驻进程用 PowerShell 实现（scripts/uiahost.ps1），本文件只做协议封装。
//
// 用法：
//   const host = await getUiaHost()          // 进程内单例，自动复用
//   await host.read(hwnd)                    // { question, options[], ticked }
//   await host.set(hwnd, 'ACDE')             // 幂等设置，逐项点+每轮重读
//   await host.post(hwnd, [[x,y], ...])      // PostMessage 点击
//   await host.act(hwnd, '交卷')              // 按名字走 UIA pattern
//   await host.tree(hwnd, 500)
import { spawn } from 'node:child_process'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
// 脚本在 scripts/ 下，本模块在 src/ 下
const HOST_SCRIPT = path.join(HERE, '..', 'scripts', 'uiahost.ps1')

export class UiaHostClient {
  constructor(script = HOST_SCRIPT, timeoutMs = 20000) {
    this.timeoutMs = timeoutMs
    this.buf = ''
    this.waiters = []
    this.child = spawn(
      'powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script],
      { stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true },
    )
    this.child.stdout.setEncoding('utf8')
    this.child.stdout.on('data', (chunk) => {
      this.buf += chunk
      let nl
      while ((nl = this.buf.indexOf('\n')) >= 0) {
        const line = this.buf.slice(0, nl).trim()
        this.buf = this.buf.slice(nl + 1)
        if (!line) continue
        const waiter = this.waiters.shift()
        if (waiter) waiter(line)
      }
    })
    this.child.on('exit', () => {
      // 进程没了：把所有挂起请求以错误收尾，别让调用方永远挂着
      for (const waiter of this.waiters.splice(0)) waiter('{"ok":false,"error":"uia host exited"}')
    })
  }

  request(payload) {
    return new Promise((resolve) => {
      const started = Date.now()
      this.waiters.push((line) => {
        let parsed
        try { parsed = JSON.parse(line) } catch { parsed = { ok: false, error: 'bad json: ' + line } }
        resolve({ ms: Date.now() - started, ...parsed })
      })
      this.child.stdin.write(JSON.stringify(payload) + '\n')
      setTimeout(() => {
        const index = this.waiters.indexOf(this.waiters[this.waiters.length - 1])
        if (index >= 0) this.waiters.splice(index, 1)
        resolve({ ms: Date.now() - started, ok: false, error: 'client timeout' })
      }, this.timeoutMs)
    })
  }

  ping() { return this.request({ op: 'ping' }) }
  read(hwnd) { return this.request({ op: 'read', hwnd }) }
  set(hwnd, target) { return this.request({ op: 'set', hwnd, target }) }
  post(hwnd, points) { return this.request({ op: 'post', hwnd, points }) }
  act(hwnd, name) { return this.request({ op: 'act', hwnd, name }) }
  dialog(hwnd) { return this.request({ op: 'dialog', hwnd }) }
  tree(hwnd, max = 500) { return this.request({ op: 'tree', hwnd, max }) }

  close() {
    try { this.child.stdin.write('{"op":"quit"}\n') } catch { }
    const child = this.child
    setTimeout(() => { try { child.kill() } catch { } }, 300)
  }
}

let singleton = null

/** The in-flight warm-up promise while the process starts; null once it has finished. */
let warming = null

/**
 * Start the host ahead of the first real call, so its ~3.5 s PowerShell start-up
 * (process spawn + UIA assembly load, measured 2026-11-01) does not land on top of a
 * tool call. Never throws: a failed warm-up just means the first real call pays the cost.
 */
export function prewarmUiaHost(script = HOST_SCRIPT) {
  if (singleton && singleton.child.exitCode === null) return Promise.resolve(singleton)
  if (warming) return warming
  warming = (async () => {
    const host = new UiaHostClient(script)
    singleton = host
    try {
      const pong = await host.ping()
      if (pong && pong.ok === true) return host
    } catch { /* caller will retry on the real call */ }
    return host
  })().finally(() => { warming = null })
  return warming
}

/** Wait for an in-flight warm-up before handing back the singleton, so the first real
 *  call is charged a fast read, not a fresh spawn. */
export async function getUiaHostReady(script = HOST_SCRIPT) {
  if (warming) { try { await warming } catch { } }
  return getUiaHost(script)
}

/** 进程内单例：第一次调用开始常驻，之后所有请求都复用它。 */
export function getUiaHost(script = HOST_SCRIPT) {
  if (!singleton || singleton.child.exitCode !== null) singleton = new UiaHostClient(script)
  return singleton
}

export function closeUiaHost() {
  if (singleton) { singleton.close(); singleton = null }
  warming = null
}
