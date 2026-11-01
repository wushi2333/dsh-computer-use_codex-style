/**
 * Page probing — read facts about a window's content from UIA text/nodes.
 *
 *   collectDocumentUrls(nodes) — live document URLs (Document-role ValuePattern) so PDF /
 *     viewer courseware can be read by URL instead of scrolled page by page.
 *   parsePageIndicator(text)   — a "current/total" counter (PDF 2/13, gallery 4/10) from a
 *     tree dump; the cheap signal that paging did or did not happen.
 *
 * Both are pure (no subprocess, no state). readPageIndicator — which needs uiaTreeFallback
 * and therefore has to stay in tool.js — imports parsePageIndicator from here. Extracted
 * verbatim from src/tool.js (P0 file split); tool.js re-exports both names.
 */
/** Extract live document URLs from a .NET UIA node list (Document role). */
export function collectDocumentUrls(nodes) {
  const seen = new Set()
  const out = []
  for (const node of Array.isArray(nodes) ? nodes : []) {
    if (!node) continue
    const role = String(node.role || '').toLowerCase()
    const value = String(node.value || '')
    if ((role === 'document' || role === '文档') && /^https?:\/\//i.test(value) && !seen.has(value)) {
      seen.add(value)
      out.push({ url: value, title: String(node.name || '') })
    }
  }
  return out
}

/**
 * Read a "current/total" page indicator out of a window's UIA text.
 *
 * `scroll_to` is wheel-driven, but wheel is not universal: the
 * moxueyuan image gallery (`/image/preview?...img-1.jpg…img-10.jpg`) ignores it
 * completely — 52 pulses ran and the indicator stayed at `4/10`. A page counter is the
 * cheap universal signal that paging has NOT happened, and it is present in exactly
 * those viewers (PDF `2/13`, gallery `4/10`, slide decks).
 *
 * Candidates inside a longer slash run (a URL such as `…/2026/01/12/…`) are rejected:
 * the match must not be preceded or followed by a digit or another slash.
 *
 * @param {string} text any text (usually the UIA fallback tree dump)
 * @returns {{current: number, total: number}|null} the widest plausible indicator
 */
export function parsePageIndicator(text) {
  const src = String(text || '')
  let best = null
  for (const m of src.matchAll(/(^|[^\d/])(\d{1,3})\s*\/\s*(\d{1,3})(?![\d/])/g)) {
    const current = Number(m[2])
    const total = Number(m[3])
    if (!Number.isFinite(current) || !Number.isFinite(total)) continue
    if (total < 2 || total > 999 || current < 1 || current > total) continue
    if (!best || total > best.total) best = { current, total }
  }
  return best
}

