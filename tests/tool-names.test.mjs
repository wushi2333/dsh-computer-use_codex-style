/**
 * P2-6: keep the documented tool names and the registered ones in step.
 *
 * `computer_use_health.tools.exposed` used to be a hardcoded list, so the 2026-10
 * `computer_use_*` rename left health printing names that no longer existed. This
 * check is the cheap guard: every `computer_use_x` a doc mentions must be a tool
 * `src/tool.js` really registers, and every tool it registers must be documented
 * somewhere — a rename that misses the docs now fails the suite instead of going out.
 *
 * Run: node --test tests/tool-names.test.mjs
 */
import assert from 'node:assert/strict'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const ROOT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..')
/** Lowercase-after-prefix only, so tokens such as COMPUTER_USE_UNAVAILABLE are not tools. */
const TOOL_NAME = /computer_use_[a-z][a-z0-9_]*/g

function walk(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === '.git' || entry.name === 'target') continue
    // `parity/` is the reverse-engineering evidence bundle: it quotes historical
    // surfaces (`computer_use_experience`, removed with the V2 experience layer) that
    // are deliberately not live tools, so it is not a user-facing tool catalog.
    if (entry.name === 'parity') continue
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) walk(full, out)
    else if (entry.name.toLowerCase().endsWith('.md')) out.push(full)
  }
  return out
}

function registeredNames() {
  const source = readFileSync(path.join(ROOT, 'src', 'tool.js'), 'utf8')
  const names = new Set()
  for (const match of source.matchAll(/name:\s*'(computer_use_[a-z][a-z0-9_]*)'/g)) names.add(match[1])
  return names
}

function documentedNames(files) {
  const names = new Map()
  for (const file of files) {
    const text = readFileSync(file, 'utf8')
    for (const match of text.match(TOOL_NAME) || []) {
      const rel = path.relative(ROOT, file).replace(/\\/g, '/')
      if (!names.has(match)) names.set(match, [])
      if (!names.get(match).includes(rel)) names.get(match).push(rel)
    }
  }
  return names
}

test('P2-6 every documented computer_use_* tool is really registered', () => {
  const docs = walk(ROOT)
  assert.ok(docs.length >= 3, `expected several markdown docs, found ${docs.length}`)
  const registered = registeredNames()
  const documented = documentedNames(docs)
  const ghosts = [...documented.keys()].filter(name => !registered.has(name))
  assert.deepEqual(
    ghosts.map(name => `${name} (in ${documented.get(name).join(', ')})`),
    [],
    'documented tool names that no longer exist in src/tool.js',
  )
})

test('P2-6 every registered tool is documented somewhere', () => {
  const registered = registeredNames()
  const documented = documentedNames(walk(ROOT))
  assert.ok(registered.size >= 7, `expected the standalone tools, found ${registered.size}`)
  const silent = [...registered].filter(name => !documented.has(name))
  assert.deepEqual(silent, [], 'registered tools with no documentation')
})

test('P2-6 the health surface no longer keeps its own hardcoded name list', () => {
  const source = readFileSync(path.join(ROOT, 'src', 'tool.js'), 'utf8')
  // The old shape was `exposed.map(...).concat([...literal names...])`.
  assert.doesNotMatch(source, /\.concat\(\['computer_use_wait'/)
  assert.match(source, /state\.exposedNames = \[\.\.\.\(state\.registeredNames \|\| \[\]\)\]/)
})