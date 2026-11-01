/**
 * CDP (Chrome DevTools Protocol) probing — decide whether a window can be driven by DOM
 * automation instead of pixels. WeChat WMPF/Radium is Chromium-derived but exposes no CDP
 * endpoint, so this says so explicitly rather than letting a caller hunt a route that
 * cannot exist.
 *
 *   listProcessRows(filter)      CIM query -> [{ProcessId,Name,ExecutablePath,CommandLine}]
 *   probeCdp(args)               is there a reachable CDP endpoint for this process/window?
 *   cachedCdpProbe(state,win)    per-app memo so a dead-tree window does not shell out on
 *                                every observation
 *
 * Depends only on drivers (powershell runner) + fs/path — no tool.js back-edge.
 * Extracted from src/tool.js (P0 split); tool.js imports cachedCdpProbe from here.
 */

import fs from 'node:fs'
import path from 'node:path'
import { runPsCommand } from '../drivers/index.js'
export function listProcessRows(filter) {
  const script = `$ErrorActionPreference='SilentlyContinue'; Get-CimInstance Win32_Process${filter} | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress`
  try {
    const raw = String(runPsCommand(script, { timeoutMs: 8000 }) || '').trim()
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : [parsed]
  } catch {
    return []
  }
}

/** Read a Chromium `DevToolsActivePort` file: line 1 is the port, line 2 the path. */
function readDevToolsActivePort(file) {
  try {
    const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/)
    const port = Number(String(lines[0] || '').trim())
    if (Number.isInteger(port) && port > 0 && port < 65536) {
      return { port, wsPath: String(lines[1] || '').trim() }
    }
  } catch { /* not present */ }
  return null
}

async function fetchCdpVersion(port) {
  try {
    const res = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(900) })
    if (!res.ok) return null
    const data = await res.json()
    return { port, browser: data?.Browser ?? null, webSocketDebuggerUrl: data?.webSocketDebuggerUrl ?? null }
  } catch {
    return null
  }
}

/**
 * Is this window's process a Chromium/WebView2 host whose CDP endpoint is reachable?
 * WeChat's mini-program runtime (WeChatAppEx / RadiumWMPF) is Chromium-derived but does
 * NOT honour --remote-debugging-port and exposes no DevToolsActivePort, so it can only
 * ever be pixel-driven — this probe says so explicitly instead of letting the model
 * keep hunting for a DOM route that cannot exist.
 */
export async function probeCdp(args) {
  let exe = ''
  if (typeof args.process === 'string' && args.process.trim()) exe = args.process.trim()
  else if (args.window && typeof args.window.app === 'string') {
    const m = /^process:(.+)$/i.exec(args.window.app)
    exe = m ? m[1] : args.window.app
  }
  let rows = []
  if (Number.isInteger(args.pid) && args.pid > 0) {
    rows = listProcessRows(` -Filter "ProcessId=${args.pid}"`)
  } else if (exe) {
    const base = path.basename(exe).replace(/'/g, "''")
    rows = listProcessRows(` -Filter "Name='${base}'"`)
    const wantPath = exe.toLowerCase()
    if (rows.some(r => r.ExecutablePath)) {
      const matched = rows.filter(r => String(r.ExecutablePath || '').toLowerCase() === wantPath)
      if (matched.length) rows = matched
    }
  }
  const ports = new Set()
  const pipePids = []
  const devToolsActivePorts = []
  for (const row of rows) {
    const cmd = String(row.CommandLine || '')
    const portMatch = /--remote-debugging-port=(\d+)/i.exec(cmd)
    if (portMatch) ports.add(Number(portMatch[1]))
    if (/--remote-debugging-pipe/i.test(cmd)) pipePids.push(Number(row.ProcessId))
    const ud = /--user-data-dir="?([^"\s]+)"?/i.exec(cmd) || /--webview-user-data-folder="?([^"\s]+)"?/i.exec(cmd)
    if (ud) {
      const found = readDevToolsActivePort(path.join(ud[1], 'DevToolsActivePort'))
      if (found) { devToolsActivePorts.push({ file: path.join(ud[1], 'DevToolsActivePort'), ...found }); ports.add(found.port) }
    }
  }
  const endpoints = []
  for (const port of ports) {
    const version = await fetchCdpVersion(port)
    if (version) endpoints.push(version)
  }
  const blob = rows.map(r => `${r.Name || ''} ${r.ExecutablePath || ''} ${r.CommandLine || ''}`).join(' ')
  const chromiumLike = /chrome|chromium|msedge|electron|webview2|WeChatAppEx|RadiumWMPF|wmpf/i.test(blob)
  const canAttach = endpoints.length > 0
  let guidance
  if (canAttach) {
    const port = endpoints[0].port
    guidance = `CDP is reachable on 127.0.0.1:${port}. Attach chrome-devtools MCP with --browserUrl http://127.0.0.1:${port} and drive the DOM instead of pixels.`
  } else if (chromiumLike) {
    guidance = exe && /WeChatAppEx|RadiumWMPF|wmpf/i.test(`${exe} ${blob}`)
      ? 'Chromium-derived host (WeChat WMPF/Radium) with NO CDP endpoint. It ignores --remote-debugging-port and writes no DevToolsActivePort, so DOM automation is not available — drive this window by screenshot + coordinates.'
      : 'Chromium/WebView2 host but no CDP endpoint is exposed right now. To enable it, relaunch that app with --remote-debugging-port=<free> (or, for a WebView2 host, set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<free> before launch), then probe again.'
  } else {
    guidance = 'The target process does not look like a Chromium/WebView2 host; CDP does not apply. Use the window2 pixel/UIA path.'
  }
  return {
    probed: { exe: exe || null, pids: rows.map(r => Number(r.ProcessId)), matchedProcesses: rows.length },
    chromiumLike,
    remoteDebuggingPorts: [...ports],
    remoteDebuggingPipePids: pipePids,
    devToolsActivePorts,
    endpoints,
    canAttach,
    guidance,
  }
}

/** Cache a CDP probe per app so a dead-tree window does not spawn a shell on every observation. */
export async function cachedCdpProbe(state, window, maxAgeMs = 60_000) {
  state.cdpProbeCache = state.cdpProbeCache || new Map()
  const key = window.app
  const hit = state.cdpProbeCache.get(key)
  const now = Date.now()
  if (hit && now - hit.at < maxAgeMs) return hit.value
  let value = null
  try { value = await probeCdp({ window }) } catch { value = null }
  state.cdpProbeCache.set(key, { at: now, value })
  return value
}


