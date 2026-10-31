/**
 * Resolve hooks for the offline unit tests.
 *
 * The plugin is normally loaded inside the DSH host, which resolves `@deepseek-ai/*`
 * from the harness checkout. Run standalone, those bare specifiers do not resolve
 * (pnpm virtual-store junctions), so map exactly the two packages `src/tool.js` touches
 * at import time to local stubs. Everything else resolves normally.
 *
 * Usage: node --import ./tests/resolve-hooks.mjs --test tests/tool-units.test.mjs
 */
import { registerHooks } from 'node:module'

const STUBS = {
  '@deepseek-ai/schemastery': new URL('./stubs/schemastery.mjs', import.meta.url).href,
  '@deepseek-ai/dsh-tools': new URL('./stubs/dsh-tools.mjs', import.meta.url).href,
}

registerHooks({
  resolve(specifier, context, nextResolve) {
    const mapped = STUBS[specifier]
    if (mapped) return { url: mapped, shortCircuit: true }
    return nextResolve(specifier, context)
  },
})