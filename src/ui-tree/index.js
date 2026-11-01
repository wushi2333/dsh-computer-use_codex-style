/**
 * UIA tree semantics: parse a .NET-ControlView tree dump into nodes, decide which nodes are
 * meaningful vs decorative containers, classify a whole tree as "dead" (all anonymous
 * containers) vs usable, and compress it down to what a reader/element-picker needs.
 *
 * Extracted verbatim from src/tool.js (P0 file split). Pure functions, no subprocess, no
 * plugin state — safe to unit test and reuse. src/tool.js re-exports these names so no
 * existing import path breaks.
 */
const GENERIC_CONTAINER_ROLES = new Set([
  'pane', '窗格', 'group', '组', 'custom', '自定义', 'region', '区域',
  'client', '客户端', 'unknown', '未知', 'document', '文档',
])
// Roles that are inherently interactive/informative even with no extra text.
const MEANINGFUL_ROLES = new Set([
  'button', '按钮', 'edit', '编辑', 'menuitem', '菜单项', 'link', '链接', 'hyperlink', '超链接',
  'checkbox', '复选框', 'radiobutton', '单选按钮', 'combobox', '组合框', 'list', '列表',
  'listitem', '列表项', 'tab', '选项卡', 'tabitem', 'text', '文本', 'image', '图像',
  'slider', '滑块', 'spinner', '微调按钮', 'treeitem', '树项目', 'menuitemcheckbox',
])
const _TREE_EXTRA_MARKERS = ['Description: ', 'Value: ', 'Secondary Actions: ', 'ID: ']

export function parseTreeLine(line) {
  const match = /^(\t*)(\d+)(?: +([^\s]+))?(.*)$/.exec(line)
  if (!match) return null
  return { indent: match[1].length, index: Number(match[2]), role: (match[3] || '').toLowerCase(), rest: (match[4] || ''), raw: line }
}

/** Does this node carry information (a name, a value/description, or a real role)? */
export function nodeIsMeaningful(node) {
  if (!node) return false
  if (MEANINGFUL_ROLES.has(node.role)) return true
  if (_TREE_EXTRA_MARKERS.some(marker => node.rest.includes(marker))) return true
  // A name: strip a leading parenthesised state block, then any trailing extras.
  let text = node.rest.trim()
  const close = text.startsWith('(') ? text.indexOf(')') : -1
  if (close !== -1) text = text.slice(close + 1).trim()
  for (const marker of _TREE_EXTRA_MARKERS) {
    const at = text.indexOf(marker)
    if (at !== -1) text = text.slice(0, at).trim()
  }
  text = text.replace(/(?:^| )(off|on|indeterminate)$/, '').trim()
  if (text) return true
  // A non-generic role with no name still hints at structure worth keeping.
  return Boolean(node.role) && !GENERIC_CONTAINER_ROLES.has(node.role)
}

/** Does this node carry actual *content* (a live value/description, not just a label)? */
// Window-chrome roles carry labels but never page content; their `Value:` (e.g. the
// title bar's window title) must NOT be mistaken for content, otherwise it masks a
// genuinely useless/anonymous content tree and blocks the .NET fallback.
const CHROME_ROLES = new Set(['titlebar', '标题栏', 'menubar', '菜单栏', 'statusbar', '状态栏'])

export function nodeHasContent(node) {
  if (!node) return false
  if (CHROME_ROLES.has(node.role)) return false
  if (node.rest.includes('Value: ') || node.rest.includes('Description: ')) return true
  // A text/document role that carries a readable name is content too.
  return (node.role === 'text' || node.role === '文本' || node.role === '文档') && nodeIsMeaningful(node)
}

/** Is this window's process a known Chromium/WebView2/Electron/WMPF host? (app string only, no shell) */
export function looksLikeChromiumHost(window) {
  const app = String(window?.app || '')
  if (!app) return false
  return /WeChatAppEx|RadiumWMPF|wmpf|msedgewebview2|webview2|electron|chrome\.exe|chromium|msedge|Microsoft\.Edge|Code\.exe|slack|discord|teams|notion|obsidian/i.test(app)
}

export function classifyAccessibilityTree(treeText, window) {
  const lines = String(treeText || '').split('\n')
  let total = 0
  let meaningful = 0
  let contentBearing = 0
  for (const line of lines) {
    const node = parseTreeLine(line)
    if (!node) continue
    total += 1
    if (nodeIsMeaningful(node)) meaningful += 1
    if (nodeHasContent(node)) contentBearing += 1
  }
  const ratio = total === 0 ? 1 : meaningful / total
  // "Dead" means: enough nodes to be a real tree, but NOT ONE content-bearing node —
  // and either the process is a known Chromium/WebView2/WMPF host (which renders into
  // a canvas and publishes only window chrome over UIA) or literally nothing is
  // meaningful. Window-chrome buttons (minimize/close) do NOT count as content, so they
  // no longer mask a useless tree.
  const dead = total >= 8 && contentBearing === 0 && (looksLikeChromiumHost(window) || meaningful <= 1)
  return { total, meaningful, contentBearing, ratio, dead }
}

/**
 * Drop decorative leaves while keeping every meaningful node and the ancestor
 * containers that lead to it. Node indices are untouched, so element_index from
 * the helper still resolves against the retained lines.
 */
export function compressAccessibilityTree(treeText) {
  const lines = String(treeText || '').split('\n')
  const nodes = []
  const nodeLineIndexes = []
  lines.forEach((line, index) => {
    const node = parseTreeLine(line)
    if (node) { nodes.push({ ...node, keep: nodeIsMeaningful(node) }); nodeLineIndexes.push(index) }
  })
  if (nodes.length === 0) return treeText
  // Propagate "has a meaningful descendant" up through the indent stack.
  const stack = []
  for (const node of nodes) {
    while (stack.length && stack[stack.length - 1].indent >= node.indent) stack.pop()
    if (node.keep) for (const ancestor of stack) ancestor.hasMeaningfulDescendant = true
    stack.push(node)
  }
  const kept = nodes.filter(node => node.keep || node.hasMeaningfulDescendant)
  if (kept.length === 0) return treeText
  const dropped = nodes.length - kept.length
  const firstNodeLine = nodeLineIndexes[0]
  const lastNodeLine = nodeLineIndexes[nodeLineIndexes.length - 1]
  const header = lines.slice(0, firstNodeLine)
  const footer = lines.slice(lastNodeLine + 1)
  const out = [...header, ...kept.map(node => node.raw), ...footer].join('\n')
  return dropped > 0
    ? `${out}\n# (semantic compression: ${dropped} anonymous container node(s) omitted; indices above are unchanged)`
    : out
}

/** One explicit verdict that replaces a useless tree. */
export function deadTreeSignal(classification, cdp) {
  const lines = [
    '=== Accessibility tree is UNUSABLE for this window (semantic verdict) ===',
    `nodes: ${classification.total}, meaningful labels: ${classification.meaningful}, content-bearing: ${classification.contentBearing} — no window content is exposed over UIA, only anonymous containers and window chrome.`,
    'This window is a Chromium/WebView2 or mini-program (WeChat WMPF/Radium) surface that does not publish its DOM over UIA.',
    'Do NOT call get_window_state with include_text for this window again (it costs tokens and yields nothing).',
    'Use screenshots + vision (vision_map / vision_read / vision_zoom) for content, and act by x/y coordinates; element_index / set_value / perform_secondary_action will not work here.',
  ]
  if (cdp) {
    if (cdp.canAttach && cdp.endpoints?.length) {
      const port = cdp.endpoints[0].port
      lines.push(`CDP IS reachable on 127.0.0.1:${port} (${cdp.endpoints[0].browser || 'chromium'}); prefer DOM automation: attach chrome-devtools MCP with --browserUrl http://127.0.0.1:${port}.`)
    } else if (cdp.chromiumLike) {
      lines.push(`CDP: ${cdp.guidance}`)
    }
  }
  return lines.join('\n')
}
