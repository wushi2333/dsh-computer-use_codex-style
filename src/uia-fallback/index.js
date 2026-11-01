/**
 * UIA text fallback for windows whose native helper tree under-reports (Chromium/WebView2/
 * WMPF/Electron all collapse to anonymous panes).
 *
 *   uiaTreeFallback(hwnd, maxNodes)  walk the .NET UIAutomation ControlView and return nodes
 *                                    with window-relative bounds (null on failure).
 *   formatUiaFallback(nodes)         render those nodes as a readable tree with {{x,y,w,h}}
 *                                    bounds an agent can act on by coordinate.
 *
 * Depends only on drivers.runScript + scripts/uia-tree.ps1 — no tool.js back-edge.
 * Extracted from src/tool.js (P0 split); tool.js re-exports both.
 */

import { runScript } from '../drivers/index.js'
export function formatUiaFallback(nodes) {
  const lines = [
    '=== UIA fallback tree (.NET UIAutomation ControlView) ===',
    'The helper-provided tree under-reported this Chromium/WebView2 window; this is a fuller walk of the same window.',
    'These nodes have NO helper element_index. Act by x/y using the window-relative bounds shown as {{x,y,w,h}}.',
    '',
  ]
  for (const n of nodes) {
    const indent = '\t'.repeat(Math.min((n.depth || 0) + 1, 12))
    const role = n.role || 'Node'
    const name = n.name ? ` ${n.name}` : ''
    const value = n.value ? ` Value: ${n.value}` : ''
    const box = ` {{x:${n.x | 0},y:${n.y | 0},w:${n.w | 0},h:${n.h | 0}}}`
    lines.push(`${indent}${role}${name}${value}${box}`)
  }
  return lines.join('\n')
}

/**
 * Walk a window's UIA tree through the bundled .NET script and return nodes with
 * window-relative bounds. This is the fix for Chromium/WebView2/mini-program windows
 * (WeChat WMPF, Electron, WebView2) whose tree the native helper under-reports down to
 * anonymous panes: a .NET UIAutomation ControlView walk DOES reach the Document/Text/
 * ListItem content and the Document value carries the live URL. Returns null on failure.
 */
export function uiaTreeFallback(hwnd, maxNodes = 800) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return null
  try {
    const raw = runScript('uia-tree.ps1', ['-Hwnd', String(hwnd), '-MaxNodes', String(maxNodes)], { timeoutMs: 20_000 })
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : [parsed]
  } catch {
    return null
  }
}

