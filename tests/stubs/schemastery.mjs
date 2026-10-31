/**
 * Minimal stand-in for `@deepseek-ai/schemastery`, used only by the offline unit
 * tests. `tool.js` builds its `Config` schema at import time, and the real package is
 * not resolvable from this checkout (pnpm virtual-store junctions), so the tests
 * substitute a builder that accepts the same chain calls and keeps the shape.
 */
function node(type) {
  const self = {
    type,
    default(value) { self._default = value; return self },
    description(value) { self._description = value; return self },
    required(value) { self._required = value; return self },
  }
  return self
}

const Schema = {
  object(shape) { const s = node('object'); s.shape = shape; return s },
  array(inner) { const s = node('array'); s.inner = inner; return s },
  string() { return node('string') },
  number() { return node('number') },
  boolean() { return node('boolean') },
  dict(inner) { const s = node('dict'); s.inner = inner; return s },
  union(values) { const s = node('union'); s.values = values; return s },
  const(value) { const s = node('const'); s.value = value; return s },
}

export default Schema