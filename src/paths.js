import path from 'node:path'
import { fileURLToPath } from 'node:url'

export function pluginRoot() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
}

/**
 * The engine root is the package root itself: the bundle layout keeps the plugin entry
 * modules (`src/`), the Rust helper (`helper-rs/`) and the Python engine (`computer_use/`)
 * side by side, exactly like the official bundle tutorial's `package.json` +
 * `cordis.patch.yml` + entry-module root.
 */
export function defaultEngineRoot() {
  return pluginRoot()
}

export function pythonCandidates(pythonPath) {
  const seen = new Set()
  const list = []
  const add = (command, prefixArgs = []) => {
    const key = `${command}\0${prefixArgs.join('\0')}`
    if (!command || seen.has(key)) return
    seen.add(key)
    list.push({ command, prefixArgs })
  }
  if (pythonPath && pythonPath.trim()) {
    const trimmed = pythonPath.trim()
    if (trimmed.toLowerCase() === 'py' || trimmed.toLowerCase().startsWith('py ')) add('py', ['-3'])
    else add(trimmed)
  }
  if (process.env.PYTHON) add(process.env.PYTHON)
  add('python')
  if (process.platform === 'win32') add('py', ['-3'])
  else add('python3')
  return list
}

export function pythonInvocation(pythonPath) {
  return pythonCandidates(pythonPath)[0]
}

export function nativeHelperCandidates(engineRoot) {
  const root = engineRoot || defaultEngineRoot()
  // `helper-rs/bin/<platform>-<arch>/` holds the TRACKED, shipped helper (see
  // `scripts/ship-helper.ps1`). It comes after the build outputs so a local `cargo build`
  // always wins during development, and it is what makes a fresh checkout -- or an install on
  // a machine without a Rust toolchain -- runnable. `engines` in package.json lists it too.
  const shipped = path.join(root, 'helper-rs', 'bin', `${process.platform}-${process.arch}`)
  const names = []
  if (process.platform === 'darwin') {
    names.push(
      path.join(root, 'helper-swift', '.build', 'release', 'dsh-computer-use'),
      path.join(root, 'helper-swift', '.build', 'debug', 'dsh-computer-use'),
      path.join(shipped, 'dsh-computer-use'),
      path.join(root, 'dsh-computer-use'),
    )
  } else if (process.platform === 'linux') {
    const shippedLinux = path.join(root, 'helper-linux', 'bin', `${process.platform}-${process.arch}`)
    names.push(
      path.join(root, 'helper-linux', 'target', 'release', 'dsh-computer-use'),
      path.join(root, 'helper-linux', 'target', 'debug', 'dsh-computer-use'),
      path.join(shippedLinux, 'dsh-computer-use'),
      path.join(root, 'helper-linux', 'bin', 'linux-x64', 'dsh-computer-use'),
      path.join(root, 'dsh-computer-use'),
    )
  } else {
    names.push(
      path.join(root, 'helper-rs', 'target', 'release', 'dsh-computer-use.exe'),
      path.join(root, 'helper-rs', 'target', 'debug', 'dsh-computer-use.exe'),
      path.join(shipped, 'dsh-computer-use.exe'),
      path.join(root, 'dsh-computer-use.exe'),
    )
  }
  if (process.env.DSH_COMPUTER_USE_HELPER) names.unshift(process.env.DSH_COMPUTER_USE_HELPER)
  return names
}
