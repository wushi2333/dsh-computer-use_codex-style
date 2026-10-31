/**
 * Minimal stand-in for `@deepseek-ai/dsh-tools`: the unit tests only need the tool
 * definitions this plugin builds, never the host registry.
 */
export function defineTool(spec) {
  return spec
}

export default { defineTool }