/**
 * Shared, dependency-free primitives every tool and helper reaches for.
 *
 * These were scattered at the top of tool.js and imported by every *Tool factory — which
 * is exactly what made "move each tool to its own file" impossible without a cycle: the
 * factories needed these symbols but they lived inside the god-file. Lifting them here
 * gives the tool factories a base they can import directly, with no back-edge into tool.js.
 *
 * Nothing in this file imports from anywhere else in the plugin (no tool.js, no
 * capture/probing/ui-tree). Keep it that way — it is the bottom of the dependency graph.
 */

/** The open output schema every tool's `output` uses (see the note below). */
export const OUTPUT_SCHEMA = {
  // `value` carries arbitrary tool output (object, array, string, number...), so the
  // schema is intentionally open. The host validator rejects an untyped `properties.value`
  // (both `{}` and `{type:'json'}` fail), so we declare no properties at all and accept
  // any object shape via additionalProperties.
  type: 'object',
  additionalProperties: true,
}

/** The `window` parameter shape: { app, id, title? }. */
export const WIN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    app: { type: 'string', required: true },
    id: { type: 'integer', required: true },
    title: { type: 'string' },
  },
}

/** A `window` parameter (required) built from {@link WIN_SCHEMA}. */
export const WIN_PARAM = { ...WIN_SCHEMA, required: true }

/** Sleep ms, but reject early if the tool call's AbortSignal fires. */
export function sleepRespectingSignal(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) { reject(new Error('wait aborted')); return }
    const timer = setTimeout(() => { cleanup(); resolve() }, ms)
    const onAbort = () => { cleanup(); reject(new Error('wait aborted')) }
    const cleanup = () => { clearTimeout(timer); signal?.removeEventListener?.('abort', onAbort) }
    signal?.addEventListener?.('abort', onAbort)
  })
}
