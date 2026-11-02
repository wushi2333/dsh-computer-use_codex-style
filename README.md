# dsh-computer-use — 已停用 / Archived

**Status: archived on 2026-11-02. This repository is read-only and no longer maintained.**

本仓库是 Codex 风格 `window2` 桌面 Computer Use 在 DeepSeek Harness 上的社区实现（Windows，MIT，
衍生自 [wushi2333/dsh-computer-use_codex-style](https://github.com/wushi2333/dsh-computer-use_codex-style)）。
它已在本机退役：

- 插件已从 DeepSeek Harness 的宿主配置、预设、技能与 submodule 中移除；
- 替代方案是 [trycua/cua](https://github.com/trycua/cua) 的 **Cua Driver**，通过 Harness 自带的
  `@deepseek-ai/dsh-experimental-computer-use-cua-driver-mcp` 提供方以 MCP 接入；
- 源码与提交历史仍保留在本仓库的 git 历史中（归档后只读，可继续 `git log` / 检出旧版本）。

Archived: this Codex-style Windows Computer Use plugin for DeepSeek Harness is retired. It was removed
from the host composition, presets, skills and the client-config submodule on 2026-11-02 and replaced by
the Cua Driver MCP integration from [trycua/cua](https://github.com/trycua/cua). Full source and history
remain available in this repository's git history.

## 归档说明 / Why

| 项 | 说明 |
|---|---|
| 退役原因 | 自研 helper + PowerShell UIA 栈维护成本高；上游 CUA Driver 提供跨平台、可独立升级的工业级 MCP 实现 |
| 替代入口 | `@deepseek-ai/dsh-computer-use`（注册服务）+ `@deepseek-ai/dsh-experimental-computer-use-cua-driver-mcp`（`cua-driver mcp`） |
| 保留内容 | 仅 git 历史；不再发布 npm 包，不再接受 issue / PR |

## 许可 / License

MIT（见 [LICENSE](LICENSE)）。归档不改变历史版本的许可。